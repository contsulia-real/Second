#![allow(dead_code)]

use std::ffi::OsString;
use std::fs;
use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use ed25519_dalek::{Signer, SigningKey};
use second::{
    AccountAddress, AuthorizerSet, BftPhase, BftQuorumCertificate, BftStatement, BftValue, BftVote,
    CURRENT_PROTOCOL_VERSION, CertifiedValidatorSetTransition, ConsensusScope, ExecutionError,
    ExecutionOutcome, FinalityCertificate, FinalityStatement, LegalTask, LegalTaskPayload,
    NodeRuntime, Operation, PaymentAddress, PeerRecord, PreparationError, PreparationOutcome,
    PreparedTaskBook, QuicClient, QuicServer, QuicTransportIdentity, SecondState, StateStore,
    TaskId, ValidatorAdmissionRequest, ValidatorCredential, ValidatorId, ValidatorRegistry,
    ValidatorSet, ValidatorSetTransition, ValidatorVote, VerifiedLegalTask,
    VerifiedValidatorAdmission,
};

pub fn key(byte: u8) -> SigningKey {
    SigningKey::from_bytes(&[byte; 32])
}

fn deterministic_address_bytes(value: u64) -> [u8; 32] {
    let mut bytes = [0_u8; 32];
    bytes[24..].copy_from_slice(&value.to_be_bytes());
    bytes
}

pub fn account(value: u64) -> AccountAddress {
    AccountAddress::from_bytes(deterministic_address_bytes(value))
}

pub fn payment(value: u64) -> PaymentAddress {
    PaymentAddress::from_bytes(deterministic_address_bytes(value))
}

pub fn task_id(value: u128) -> TaskId {
    TaskId::parse(&format!("t{value:032x}")).unwrap()
}

pub fn verified_task(task_number: u128, operations: Vec<Operation>) -> VerifiedLegalTask {
    verified_task_with_expiry(task_number, None, operations)
}

pub fn verified_task_with_expiry(
    task_number: u128,
    expires_at: Option<u64>,
    operations: Vec<Operation>,
) -> VerifiedLegalTask {
    let signing = key(9);
    let authorizers = AuthorizerSet::new(
        CURRENT_PROTOCOL_VERSION,
        [signing.verifying_key().to_bytes()],
    )
    .unwrap();
    let payload = LegalTaskPayload::new(
        task_id(task_number),
        CURRENT_PROTOCOL_VERSION,
        expires_at,
        operations,
    );

    LegalTask::sign(payload, &signing)
        .unwrap()
        .verify(&authorizers)
        .unwrap()
}

pub fn payment_address(account: AccountAddress) -> PaymentAddress {
    PaymentAddress::from_bytes(account.bytes())
}

pub fn register_payment_addresses(
    state: &mut SecondState,
    accounts: impl IntoIterator<Item = AccountAddress>,
) {
    static NEXT_PAYMENT_TASK: AtomicU64 = AtomicU64::new(0);
    let operations = accounts
        .into_iter()
        .map(|account| Operation::RegisterPaymentAddress {
            address: payment_address(account),
            account,
        })
        .collect::<Vec<_>>();
    if operations.is_empty() {
        return;
    }

    let task_number = u128::MAX - u128::from(NEXT_PAYMENT_TASK.fetch_add(1, Ordering::Relaxed));
    let task = verified_task(task_number, operations);
    let mut harness = FinalityHarness::new("register-payment-addresses");
    assert_eq!(
        harness.execute(state, &task, 0).unwrap(),
        ExecutionOutcome::Succeeded
    );
}

pub fn validator_credential(id: u64) -> ValidatorCredential {
    ValidatorCredential::new(
        ValidatorId::new(id),
        key((id * 3) as u8).verifying_key().to_bytes(),
        key((id * 3 + 1) as u8).verifying_key().to_bytes(),
        key((id * 3 + 2) as u8).verifying_key().to_bytes(),
    )
    .unwrap()
}

pub fn validator_set(version: u64, ids: impl IntoIterator<Item = u64>) -> ValidatorSet {
    ValidatorSet::new(version, ids.into_iter().map(validator_credential)).unwrap()
}

pub fn verified_validator_admission(id: u64) -> VerifiedValidatorAdmission {
    ValidatorAdmissionRequest::sign(
        CURRENT_PROTOCOL_VERSION,
        validator_credential(id),
        &key((id * 3) as u8),
        &key((id * 3 + 1) as u8),
        &key((id * 3 + 2) as u8),
    )
    .unwrap()
    .verify()
    .unwrap()
}

pub fn certified_add_validator_transition(
    current: &ValidatorSet,
    next_version: u64,
    existing_ids: impl IntoIterator<Item = u64>,
    new_validator_id: u64,
    signer_ids: impl IntoIterator<Item = u64>,
) -> CertifiedValidatorSetTransition {
    let registry = ValidatorRegistry::from_validator_set(current).unwrap();
    let mut credentials = existing_ids
        .into_iter()
        .map(validator_credential)
        .collect::<Vec<_>>();
    credentials.push(validator_credential(new_validator_id));
    let next = ValidatorSet::new(next_version, credentials).unwrap();
    let transition = ValidatorSetTransition::new(
        CURRENT_PROTOCOL_VERSION,
        current,
        &registry,
        next,
        vec![verified_validator_admission(new_validator_id)],
        Vec::new(),
    )
    .unwrap();
    certify_validator_transition(current, transition, signer_ids)
}

pub fn certify_validator_transition(
    current: &ValidatorSet,
    transition: ValidatorSetTransition,
    signer_ids: impl IntoIterator<Item = u64>,
) -> CertifiedValidatorSetTransition {
    let statement = transition.finality_statement();
    let votes = signer_ids
        .into_iter()
        .map(|id| {
            let validator_id = ValidatorId::new(id);
            signed_vote(
                &statement,
                validator_id,
                &key((validator_id.value() * 3 + 1) as u8),
            )
        })
        .collect();

    CertifiedValidatorSetTransition::new(transition, votes, current).unwrap()
}

pub fn certify_and_activate_validator_transition(
    current: &ValidatorSet,
    registry: &mut ValidatorRegistry,
    transition: ValidatorSetTransition,
    signer_ids: impl IntoIterator<Item = u64>,
) -> ValidatorSet {
    certify_validator_transition(current, transition, signer_ids)
        .activate(registry)
        .unwrap()
}

pub fn signed_vote(
    statement: &FinalityStatement,
    validator_id: ValidatorId,
    signing_key: &SigningKey,
) -> ValidatorVote {
    let signature = signing_key
        .sign(&statement.canonical_signing_bytes())
        .to_bytes();
    ValidatorVote::from_untrusted_parts(validator_id, signature)
}

pub fn signed_bft_vote(statement: &BftStatement, validator_id: ValidatorId) -> BftVote {
    let signing_key = key((validator_id.value() * 3 + 1) as u8);
    BftVote::from_untrusted_parts(
        validator_id,
        signing_key
            .sign(&statement.canonical_signing_bytes())
            .to_bytes(),
    )
}

pub fn bft_qc(
    scope: ConsensusScope,
    round: u64,
    phase: BftPhase,
    value: BftValue,
    signers: impl IntoIterator<Item = u64>,
    validator_set: &ValidatorSet,
) -> BftQuorumCertificate {
    let statement = BftStatement::new(validator_set.version(), scope, round, phase, value);
    let votes = signers
        .into_iter()
        .map(|id| signed_bft_vote(&statement, ValidatorId::new(id)))
        .collect();
    BftQuorumCertificate::new(statement, votes, validator_set).unwrap()
}

pub fn mark_bft_finality_ready(
    store: &StateStore,
    validator_id: ValidatorId,
    scope: second::ConsensusScope,
    digest: [u8; 32],
    validator_set: &ValidatorSet,
    signers: impl IntoIterator<Item = (ValidatorId, SigningKey)>,
) {
    let statement = second::BftStatement::new(
        validator_set.version(),
        scope,
        0,
        second::BftPhase::Precommit,
        second::BftValue::Digest(digest),
    );
    let message = statement.canonical_signing_bytes();
    let votes = signers
        .into_iter()
        .map(|(signer_id, signing_key)| {
            second::BftVote::from_untrusted_parts(signer_id, signing_key.sign(&message).to_bytes())
        })
        .collect();
    let certificate = second::BftQuorumCertificate::new(statement, votes, validator_set).unwrap();
    store
        .accept_bft_precommit_qc(validator_id, &certificate, validator_set)
        .unwrap();
}

pub fn certificate_from_keys(
    statement: FinalityStatement,
    validator_set: &ValidatorSet,
    signers: impl IntoIterator<Item = (ValidatorId, SigningKey)>,
) -> FinalityCertificate {
    let votes = signers
        .into_iter()
        .map(|(validator_id, signing_key)| signed_vote(&statement, validator_id, &signing_key))
        .collect();
    FinalityCertificate::new(statement, votes, validator_set).unwrap()
}

pub struct FinalityHarness {
    pub book: PreparedTaskBook,
    pub validators: ValidatorSet,
    store: StateStore,
}

impl FinalityHarness {
    pub fn new(name: &str) -> Self {
        Self::with_validator_version(name, 7)
    }

    pub fn with_validator_version(name: &str, version: u64) -> Self {
        let store = StateStore::new(temp_base(name));
        Self {
            book: PreparedTaskBook::new(store.clone()).unwrap(),
            validators: validator_set(version, 1..=4),
            store,
        }
    }

    pub fn prepare(
        &mut self,
        state: &mut SecondState,
        task: &VerifiedLegalTask,
        now: u64,
    ) -> Result<PreparationOutcome, PreparationError> {
        self.book.prepare(state, task, now, &self.validators)
    }

    pub fn commit(
        &mut self,
        state: &mut SecondState,
        task_id: TaskId,
    ) -> Result<ExecutionOutcome, PreparationError> {
        let statement = self.book.prepared_finality_statement(task_id.clone())?;
        let signers = [1_u64, 2, 3]
            .into_iter()
            .map(|id| (ValidatorId::new(id), key((id * 3 + 1) as u8)));
        let certificate = certificate_from_keys(statement, &self.validators, signers);
        self.book.commit(state, task_id, &certificate)
    }

    pub fn execute(
        &mut self,
        state: &mut SecondState,
        task: &VerifiedLegalTask,
        now: u64,
    ) -> Result<ExecutionOutcome, PreparationError> {
        match self.prepare(state, task, now)? {
            PreparationOutcome::Prepared => self.commit(state, task.task_id()),
            PreparationOutcome::AlreadySucceeded => Ok(ExecutionOutcome::AlreadySucceeded),
        }
    }

    pub fn cancel(&mut self, task_id: TaskId) -> Result<(), PreparationError> {
        self.book.cancel(task_id)
    }
    pub fn claimed_currency_count(&self) -> usize {
        self.book.claimed_currency_count()
    }

    pub fn prepared_count(&self) -> usize {
        self.book.prepared_count()
    }
}

impl Drop for FinalityHarness {
    fn drop(&mut self) {
        let _ = self.store.remove_files();
    }
}

pub trait FinalizedExecute {
    fn execute_finalized(
        &mut self,
        task: &VerifiedLegalTask,
        now: u64,
    ) -> Result<ExecutionOutcome, ExecutionError>;
}

impl FinalizedExecute for SecondState {
    fn execute_finalized(
        &mut self,
        task: &VerifiedLegalTask,
        now: u64,
    ) -> Result<ExecutionOutcome, ExecutionError> {
        let mut harness = FinalityHarness::new("finalized-execute");
        match harness.execute(self, task, now) {
            Ok(outcome) => Ok(outcome),
            Err(PreparationError::Execution(error)) => Err(error),
            Err(PreparationError::Claim(second::ClaimError::InsufficientBalance {
                account,
                required,
                available,
            })) => Err(ExecutionError::InsufficientBalance {
                account,
                required,
                available,
            }),
            Err(PreparationError::Claim(second::ClaimError::ReserveUnavailable {
                required,
                available,
            })) => Err(ExecutionError::ReserveUnavailable {
                required,
                available,
            }),
            Err(error) => {
                panic!("finalized test execution failed outside domain error path: {error:?}")
            }
        }
    }
}

pub fn quic_server() -> (QuicServer, Vec<u8>) {
    let identity = QuicTransportIdentity::generate().unwrap();
    let certificate = identity.certificate_der().to_vec();
    let server = QuicServer::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)), &identity).unwrap();
    (server, certificate)
}

pub fn quic_client(server_certificate: &[u8]) -> QuicClient {
    QuicClient::new(
        SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)),
        server_certificate,
        QuicTransportIdentity::generate().unwrap(),
    )
    .unwrap()
}

pub fn node_runtime_fixture(prefix: &str) -> (Arc<NodeRuntime>, StateStore, PathBuf) {
    let base = temp_base(prefix);
    let store = StateStore::new(&base);
    let state = SecondState::genesis([], 1).with_reserve(2).unwrap();
    store.initialize(&state, &validator_set(1, 1..=4)).unwrap();

    let runtime = Arc::new(
        NodeRuntime::load_and_bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)), &store).unwrap(),
    );
    (runtime, store, base)
}

pub fn peer_record(runtime: &NodeRuntime) -> PeerRecord {
    runtime
        .local_peer_record()
        .cloned()
        .expect("test runtime must have an advertisable peer record")
}

pub fn spawn_node_runtime(runtime: &Arc<NodeRuntime>) -> tokio::task::JoinHandle<()> {
    spawn_node_runtime_with_bootstrap(runtime, Vec::new())
}

pub fn spawn_node_runtime_with_bootstrap(
    runtime: &Arc<NodeRuntime>,
    bootstrap_records: Vec<PeerRecord>,
) -> tokio::task::JoinHandle<()> {
    let runtime = Arc::clone(runtime);
    tokio::spawn(async move {
        runtime
            .run(&bootstrap_records)
            .await
            .expect("test node runtime must keep running");
    })
}

pub fn cleanup_node_runtime(store: StateStore, base: PathBuf) {
    store.remove_files().unwrap();
    remove_transport_identity(&base);
    remove_optional_file(peer_store_path(&base), "peer store");
    remove_optional_file(bootstrap_config_path(&base), "bootstrap config");
}

pub fn temp_base(prefix: &str) -> PathBuf {
    static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

    let unique = format!(
        "second-{prefix}-{}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock must be after unix epoch")
            .as_nanos(),
        NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed),
    );
    std::env::temp_dir().join(unique)
}

pub fn transport_identity_path(base: &Path) -> PathBuf {
    let mut path = OsString::from(base.as_os_str());
    path.push(".transport");
    PathBuf::from(path)
}

pub fn peer_store_path(base: &Path) -> PathBuf {
    append_suffix(base, ".peers")
}

pub fn bootstrap_config_path(base: &Path) -> PathBuf {
    append_suffix(base, ".bootstrap.json")
}

pub fn remove_transport_identity(base: &Path) {
    let identity = transport_identity_path(base);
    for path in [
        identity.clone(),
        append_suffix(&identity, ".lock"),
        append_suffix(&identity, ".new"),
    ] {
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => panic!("failed to remove transport identity artifact: {error}"),
        }
    }
}

fn remove_optional_file(path: PathBuf, description: &str) {
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => panic!("failed to remove {description} artifact: {error}"),
    }
}

fn append_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut value = OsString::from(path.as_os_str());
    value.push(suffix);
    PathBuf::from(value)
}
