#![allow(dead_code)]

pub mod allocation_diagnostics;
pub mod progress;

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs;
use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use ed25519_dalek::{Signer, SigningKey};
use second::{
    AccountAddress, AuthorizerSet, BftPhase, BftQuorumCertificate, BftStatement, BftTimeoutConfig,
    BftValue, BftVote, CURRENT_PROTOCOL_VERSION, CertifiedValidatorSetTransition, ConsensusScope,
    ExecutionError, ExecutionOutcome, FinalityCertificate, FinalityStatement, LegalTask,
    LegalTaskPayload, NodeRuntime, Operation, PaymentAddress, PeerRecord, PreparationError,
    PreparationOutcome, PreparedTaskBook, PublicStateStore, QuicClient, QuicServer,
    QuicTransportIdentity, SecondState, StateStore, TaskId, ValidatorAdmissionRequest,
    ValidatorCredential, ValidatorId, ValidatorRegistry, ValidatorRuntimeConfig,
    ValidatorRuntimeKeys, ValidatorSet, ValidatorSetTransition, ValidatorVote, VerifiedLegalTask,
    VerifiedValidatorAdmission,
};
use serde_json::json;

pub fn key(byte: u8) -> SigningKey {
    SigningKey::from_bytes(&[byte; 32])
}

pub fn single_validator_set(
    version: u64,
    validator_id: u64,
    identity_seed: u8,
    consensus_seed: u8,
    recovery_seed: u8,
) -> ValidatorSet {
    ValidatorSet::new(
        version,
        [ValidatorCredential::new(
            ValidatorId::new(validator_id),
            key(identity_seed).verifying_key().to_bytes(),
            key(consensus_seed).verifying_key().to_bytes(),
            key(recovery_seed).verifying_key().to_bytes(),
        )
        .unwrap()],
    )
    .unwrap()
}

pub fn write_validator_sidecars(
    base: &Path,
    validator_id: u64,
    identity_seed: u8,
    consensus_seeds: &[u8],
    recovery_seed: u8,
    authorizer_public_keys: &[[u8; 32]],
) -> (PathBuf, PathBuf) {
    let config_path = append_suffix(base, ".validator.json");
    let keyring_path = append_suffix(base, ".validator.keys.json");

    let config = json!({
        "network_id_base64": STANDARD.encode([0_u8; 32]),
        "authorizer_public_keys_base64": authorizer_public_keys
            .iter()
            .map(|key| STANDARD.encode(key))
            .collect::<Vec<_>>(),
        "bft_timeouts_ms": {
            "proposal": 250,
            "prevote": 250,
            "precommit": 250
        }
    });
    fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();

    let keyring = json!({
        "validator_id": validator_id,
        "identity_private_key_base64": STANDARD.encode(key(identity_seed).to_bytes()),
        "recovery_private_key_base64": STANDARD.encode(key(recovery_seed).to_bytes()),
        "consensus_private_keys_base64": consensus_seeds
            .iter()
            .map(|seed| STANDARD.encode(key(*seed).to_bytes()))
            .collect::<Vec<_>>()
    });
    fs::write(&keyring_path, serde_json::to_vec(&keyring).unwrap()).unwrap();
    secure_private_file_permissions(&keyring_path);

    (config_path, keyring_path)
}

#[cfg(unix)]
fn secure_private_file_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt as _;

    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

#[cfg(not(unix))]
fn secure_private_file_permissions(_path: &Path) {}

pub fn authorizers() -> AuthorizerSet {
    AuthorizerSet::new(
        CURRENT_PROTOCOL_VERSION,
        [key(9).verifying_key().to_bytes()],
    )
    .unwrap()
}

pub fn validator_runtime_config(timeouts: BftTimeoutConfig) -> ValidatorRuntimeConfig {
    ValidatorRuntimeConfig::new(authorizers(), timeouts, || 1)
}

pub fn default_validator_runtime_config() -> ValidatorRuntimeConfig {
    validator_runtime_config(BftTimeoutConfig::new(
        Duration::from_secs(2),
        Duration::from_secs(2),
        Duration::from_secs(2),
    ))
}

fn deterministic_address_bytes(value: u64) -> [u8; 32] {
    let mut bytes = [0_u8; 32];
    bytes[24..].copy_from_slice(&value.to_be_bytes());
    bytes
}

pub fn account_key(value: u64) -> SigningKey {
    let mut seed = deterministic_address_bytes(value);
    seed[0] = 0x53;
    SigningKey::from_bytes(&seed)
}

fn account_registry() -> &'static Mutex<BTreeMap<AccountAddress, u64>> {
    static REGISTRY: OnceLock<Mutex<BTreeMap<AccountAddress, u64>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn payment_registry() -> &'static Mutex<BTreeMap<PaymentAddress, BTreeSet<u64>>> {
    static REGISTRY: OnceLock<Mutex<BTreeMap<PaymentAddress, BTreeSet<u64>>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(BTreeMap::new()))
}

pub fn account(value: u64) -> AccountAddress {
    let address = AccountAddress::from_bytes(account_key(value).verifying_key().to_bytes());
    account_registry().lock().unwrap().insert(address, value);
    address
}

/// Test-only real account signatures. No validator bypass or fake identities.
pub fn sign_task(
    payload: LegalTaskPayload,
    authorizer: &SigningKey,
) -> Result<LegalTask, second::TaskEncodingError> {
    sign_task_in_network(payload, authorizer, [0; 32])
}

pub fn sign_task_in_network(
    payload: LegalTaskPayload,
    authorizer: &SigningKey,
    network_id: [u8; 32],
) -> Result<LegalTask, second::TaskEncodingError> {
    let known = account_registry().lock().unwrap().clone();
    let mut owners = BTreeSet::new();
    let mut payments = payment_registry().lock().unwrap();
    for operation in payload.operations() {
        match operation {
            Operation::RegisterAccount { account } => {
                owners.insert(*account);
            }
            Operation::RegisterPaymentAddress { address, account } => {
                owners.insert(*account);
                if let Some(seed) = known.get(account) {
                    payments.entry(*address).or_default().insert(*seed);
                }
            }
            Operation::Transfer { source, .. }
            | Operation::RetirePaymentAddress { address: source }
            | Operation::FinalizePaymentAddressRetirement { address: source } => {
                if let Some(seed) = known.get(&AccountAddress::from_bytes(source.bytes())) {
                    owners.insert(account(*seed));
                }
                if let Some(seeds) = payments.get(source) {
                    for seed in seeds {
                        owners.insert(account(*seed));
                    }
                }
            }
            Operation::LeakRepair { .. } => {
                owners.insert(account(1));
            }
            Operation::Issue { .. } | Operation::Destroy { .. } => {}
        }
    }
    drop(payments);
    let mut task = LegalTask::sign_in_network(payload, authorizer, network_id)?;
    for owner in owners {
        if let Some(seed) = known.get(&owner) {
            task.add_account_signature(&account_key(*seed))?;
        }
    }
    Ok(task)
}

pub fn payment(value: u64) -> PaymentAddress {
    PaymentAddress::from_bytes(deterministic_address_bytes(value))
}

pub fn task_id(value: u128) -> TaskId {
    TaskId::parse(&format!("t{value:032x}")).unwrap()
}

pub fn write_issue_transaction_request(
    path: &Path,
    recipient: AccountAddress,
    task_number: u128,
    authorizer: &SigningKey,
    count: u64,
) -> TaskId {
    write_transaction_request(
        path,
        task_number,
        authorizer,
        vec![Operation::Issue {
            account: recipient,
            count,
        }],
        json!([{ "type": "issue", "recipient": recipient.to_string(), "amount": count }]),
    )
}

pub fn write_transaction_request(
    path: &Path,
    task_number: u128,
    authorizer: &SigningKey,
    operations: Vec<Operation>,
    json_operations: serde_json::Value,
) -> TaskId {
    write_transaction_request_in_network(
        path,
        task_number,
        authorizer,
        operations,
        json_operations,
        [0; 32],
    )
}

pub fn write_transaction_request_in_network(
    path: &Path,
    task_number: u128,
    authorizer: &SigningKey,
    operations: Vec<Operation>,
    json_operations: serde_json::Value,
    network_id: [u8; 32],
) -> TaskId {
    let task_id = task_id(task_number);
    let task = sign_task_in_network(
        LegalTaskPayload::new(task_id.clone(), CURRENT_PROTOCOL_VERSION, None, operations),
        authorizer,
        network_id,
    )
    .unwrap();
    let request = json!({
        "request_id": task_id.as_str(),
        "version": CURRENT_PROTOCOL_VERSION,
        "expires_at": null,
        "network_id_base64": STANDARD.encode(network_id),
        "operations": json_operations,
        "signature": task.signature_base64url(),
        "account_signatures": task.account_signatures().iter().map(|entry| json!({"account":entry.account.to_string(), "signature": base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(entry.signature)})).collect::<Vec<_>>()
    });
    fs::write(path, serde_json::to_vec(&request).unwrap()).unwrap();
    task_id
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
    let authorizers = authorizers();
    let payload = LegalTaskPayload::new(
        task_id(task_number),
        CURRENT_PROTOCOL_VERSION,
        expires_at,
        operations,
    );

    sign_task(payload, &signing)
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

pub fn certified_validator_membership_transition(
    current: &ValidatorSet,
    next_version: u64,
    next_ids: impl IntoIterator<Item = u64>,
    admitted_ids: impl IntoIterator<Item = u64>,
    signer_ids: impl IntoIterator<Item = u64>,
    currency_frontier: u64,
) -> CertifiedValidatorSetTransition {
    let registry = ValidatorRegistry::from_validator_set(current).unwrap();
    let next =
        ValidatorSet::new(next_version, next_ids.into_iter().map(validator_credential)).unwrap();
    let admissions = admitted_ids
        .into_iter()
        .map(verified_validator_admission)
        .collect();
    let transition = ValidatorSetTransition::new(
        CURRENT_PROTOCOL_VERSION,
        current,
        &registry,
        next,
        admissions,
        Vec::new(),
        currency_frontier,
    )
    .unwrap();
    certify_validator_transition(current, transition, signer_ids)
}

pub fn certified_add_validator_transition(
    current: &ValidatorSet,
    next_version: u64,
    existing_ids: impl IntoIterator<Item = u64>,
    new_validator_id: u64,
    signer_ids: impl IntoIterator<Item = u64>,
    currency_frontier: u64,
) -> CertifiedValidatorSetTransition {
    let mut next_ids = existing_ids.into_iter().collect::<Vec<_>>();
    next_ids.push(new_validator_id);
    certified_validator_membership_transition(
        current,
        next_version,
        next_ids,
        [new_validator_id],
        signer_ids,
        currency_frontier,
    )
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
        allocate_task(&self.store, state, task, now, &self.validators)?;
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
    pub fn claimed_currency_count(&self) -> u64 {
        self.book.claimed_currency_count()
    }

    pub fn prepared_count(&self) -> usize {
        self.book.prepared_count()
    }
}

pub fn allocate_task(
    store: &StateStore,
    state: &mut SecondState,
    task: &VerifiedLegalTask,
    now: u64,
    validators: &ValidatorSet,
) -> Result<(), PreparationError> {
    if task.is_expired(now)
        || !task.operations().iter().any(|operation| {
            matches!(operation, Operation::Issue { count, .. } if *count > 0)
                || matches!(operation, Operation::LeakRepair { leaked } if !leaked.is_empty())
        })
    {
        return Ok(());
    }
    if store.load().unwrap().is_none() {
        store.initialize(state, validators).unwrap();
    }
    let allocation =
        second::CurrencyAllocation::new(task, validators.version(), state.next_currency_address())
            .map_err(PreparationError::Execution)?;
    // All fixture keys use repeated-byte seeds. Resolve the actual credential instead
    // of assuming that every test uses the same validator key layout.
    let signers = validators
        .credentials()
        .take(validators.quorum_threshold())
        .map(|credential| {
            let signing = (0..=255)
                .map(key)
                .find(|signing| {
                    signing.verifying_key().to_bytes() == credential.consensus_public_key()
                })
                .unwrap();
            (credential.id(), signing)
        });
    let certificate = certificate_from_keys(allocation.finality_statement(), validators, signers);
    match store.install_currency_allocation(&allocation, &certificate) {
        Ok(()) => *state = store.load().unwrap().unwrap().state,
        Err(second::PersistenceError::StaleState) => {}
        Err(second::PersistenceError::SnapshotTooLarge) => {
            return Err(PreparationError::Execution(
                ExecutionError::CurrencyAllocationFailed {
                    requested: allocation.count(),
                },
            ));
        }
        Err(error) => return Err(error.into()),
    }
    Ok(())
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
    remove_optional_file(append_suffix(&base, ".runtime.lock"), "runtime lock");
}

pub fn cleanup_public_node_runtime(store: PublicStateStore, base: PathBuf) {
    store.remove_files().unwrap();
    remove_transport_identity(&base);
    remove_optional_file(peer_store_path(&base), "peer store");
    remove_optional_file(bootstrap_config_path(&base), "bootstrap config");
    remove_optional_file(append_suffix(&base, ".runtime.lock"), "runtime lock");
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

pub(crate) fn append_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut value = OsString::from(path.as_os_str());
    value.push(suffix);
    PathBuf::from(value)
}

pub fn bind_validator_runtime(
    store: &StateStore,
    keys: second::ValidatorRuntimeKeys,
    config: ValidatorRuntimeConfig,
) -> NodeRuntime {
    let persisted = store.load().unwrap().unwrap();
    NodeRuntime::bind_loaded(
        SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
        store,
        persisted,
        second::NodeRuntimeCapabilities::default().with_validator(keys, config),
    )
    .unwrap()
}

pub fn validator_runtime_fixture(
    prefix: &str,
    validator_id: u64,
) -> (Arc<NodeRuntime>, StateStore, PathBuf) {
    validator_runtime_fixture_with_timeouts(
        prefix,
        validator_id,
        BftTimeoutConfig::new(
            Duration::from_secs(2),
            Duration::from_secs(2),
            Duration::from_secs(2),
        ),
    )
}

pub fn validator_runtime_fixture_with_timeouts(
    prefix: &str,
    validator_id: u64,
    timeouts: BftTimeoutConfig,
) -> (Arc<NodeRuntime>, StateStore, PathBuf) {
    let base = temp_base(prefix);
    let store = StateStore::new(&base);
    let state = SecondState::genesis([], 1).with_reserve(2).unwrap();
    store.initialize(&state, &validator_set(1, 1..=4)).unwrap();
    let runtime = Arc::new(bind_validator_runtime(
        &store,
        ValidatorRuntimeKeys::new(
            ValidatorId::new(validator_id),
            key((validator_id * 3) as u8),
            key((validator_id * 3 + 1) as u8),
        ),
        validator_runtime_config(timeouts),
    ));
    (runtime, store, base)
}
pub mod ports;

pub fn fragmented_public_state(count: u64) -> SecondState {
    let owner = account(1);
    let mut state = SecondState::genesis([owner], 10)
        .with_reserve(count)
        .unwrap();
    state
        .execute_finalized(
            &verified_task(
                80001,
                vec![second::Operation::Issue {
                    account: owner,
                    count: count * 2,
                }],
            ),
            1,
        )
        .unwrap();
    let leaked = (0..count)
        .map(|index| second::CurrencyAddress::new(10 + count + index * 2))
        .collect();
    state
        .execute_finalized(
            &verified_task(80002, vec![second::Operation::LeakRepair { leaked }]),
            1,
        )
        .unwrap();
    state
}
