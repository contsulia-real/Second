use std::collections::{BTreeMap, BTreeSet};

use sha2::{Digest, Sha256};

use crate::ConsensusScope;
use crate::currency::Currency;
use crate::payment::{PaymentAddressRecord, PaymentExecution};
use crate::prepared_plan::PreparedTask;
use crate::state::{BusinessState, PrerequisiteState, ProtocolState, TaskBinding, TaskOutcome};
use crate::{
    AccountAddress, BftLocalState, CurrencyAddress, CurrencyRole,
    MAX_PUBLIC_CURRENCY_DELTA_CHANGES, OperationClaimId, PaymentAddress, PaymentAddressStatus,
    PersistedNodeState, PersistenceError, PublicCurrencyCheckpointProof, PublicCurrencyDelta,
    SecondState, StateRecoveryCheckpointProof, StateRecoveryPayload, TaskId, ValidatorId,
    ValidatorRegistry, ValidatorSet, ValidatorSetTransitionProof,
};

use super::RecoveryCheckpointFloor;
use super::local_codec::{decode_local_state, encode_local_state};
use super::safety_recovery::{
    PendingValidatorSafetyRecovery, decode_optional_pending_safety_recovery,
    encode_optional_pending_safety_recovery,
};
use super::snapshot_validation::{
    validate_active_prepared_vote_lock_membership, validate_bft_local_state_registry,
    validate_prepared_finality_proofs, validate_prepared_plans_against_state,
    validate_prepared_snapshot_links, validate_retained_validator_sets,
    validate_vote_lock_registry,
};
use super::validator_codec::{
    decode_validator_registry, decode_validator_set, encode_validator_registry,
    encode_validator_set,
};

const SNAPSHOT_MAGIC: [u8; 4] = *b"S2SN";
const SNAPSHOT_VERSION: u32 = 1;
const SNAPSHOT_DOMAIN: &[u8] = b"SECOND_STATE_SNAPSHOT_V1\0";
pub(super) const CHECKSUM_SIZE: usize = 32;
const HEADER_SIZE: usize = 4 + 4 + 8 + 8;
const MAX_SNAPSHOT_PAYLOAD_SIZE: u64 = 512 * 1024 * 1024;

pub(super) const MAX_SNAPSHOT_FILE_SIZE: u64 =
    MAX_SNAPSHOT_PAYLOAD_SIZE + (HEADER_SIZE + CHECKSUM_SIZE) as u64;

#[derive(Clone, Copy)]
pub(super) struct SnapshotContents<'a> {
    pub(super) task_receipts: &'a super::TaskReceipts,
    pub(super) pending_governance: Option<&'a BTreeMap<[u8; 32], super::PendingGovernance>>,
    pub(super) state: &'a SecondState,
    pub(super) validator_set: &'a ValidatorSet,
    pub(super) retained_validator_sets: &'a BTreeMap<u64, ValidatorSet>,
    pub(super) public_checkpoint_proof: Option<&'a PublicCurrencyCheckpointProof>,
    pub(super) public_checkpoint_baseline: Option<&'a PublicCurrencyCheckpointProof>,
    pub(super) latest_public_delta: Option<&'a PublicCurrencyDelta>,
    pub(super) pending_public_changes: Option<&'a BTreeSet<CurrencyAddress>>,
    pub(super) validator_transition_proofs: &'a BTreeMap<u64, ValidatorSetTransitionProof>,
    pub(super) recovery_checkpoint_proof: Option<&'a StateRecoveryCheckpointProof>,
    pub(super) checkpoint_floor_epoch: u64,
    pub(super) recovery_checkpoint_floors: &'a BTreeMap<u64, RecoveryCheckpointFloor>,
    pub(super) validator_safety_ready: bool,
    pub(super) minimum_signing_validator_set_version: u64,
    pub(super) pending_validator_safety_recovery: Option<&'a PendingValidatorSafetyRecovery>,
    pub(super) validator_registry: &'a ValidatorRegistry,
    pub(super) prepared_tasks: &'a BTreeMap<TaskId, PreparedTask>,
    pub(super) validator_vote_locks: &'a BTreeMap<(ValidatorId, ConsensusScope), [u8; 32]>,
    pub(super) bft_local_states: &'a BTreeMap<(ValidatorId, ConsensusScope), BftLocalState>,
}
impl SnapshotContents<'_> {
    pub(super) fn owned_snapshot(self, generation: u64) -> PersistedNodeState {
        PersistedNodeState {
            task_receipts: self.task_receipts.clone(),
            pending_governance: self.pending_governance.cloned().unwrap_or_default(),
            generation,
            state: self.state.clone(),
            validator_set: self.validator_set.clone(),
            retained_validator_sets: self.retained_validator_sets.clone(),
            public_checkpoint_proof: self.public_checkpoint_proof.cloned(),
            public_checkpoint_baseline: self.public_checkpoint_baseline.cloned(),
            latest_public_delta: self.latest_public_delta.cloned(),
            pending_public_changes: self.pending_public_changes.cloned(),
            validator_transition_proofs: self.validator_transition_proofs.clone(),
            recovery_checkpoint_proof: self.recovery_checkpoint_proof.cloned(),
            checkpoint_floor_epoch: self.checkpoint_floor_epoch,
            recovery_checkpoint_floors: self.recovery_checkpoint_floors.clone(),
            validator_safety_ready: self.validator_safety_ready,
            minimum_signing_validator_set_version: self.minimum_signing_validator_set_version,
            pending_validator_safety_recovery: self.pending_validator_safety_recovery.cloned(),
            validator_registry: self.validator_registry.clone(),
            prepared_tasks: self.prepared_tasks.clone(),
            validator_vote_locks: self.validator_vote_locks.clone(),
            bft_local_states: self.bft_local_states.clone(),
        }
    }
}

struct DecodedSnapshotPayload {
    task_receipts: super::TaskReceipts,
    pending_governance: BTreeMap<[u8; 32], super::PendingGovernance>,
    state: SecondState,
    validator_set: ValidatorSet,
    retained_validator_sets: BTreeMap<u64, ValidatorSet>,
    public_checkpoint_proof: Option<PublicCurrencyCheckpointProof>,
    public_checkpoint_baseline: Option<PublicCurrencyCheckpointProof>,
    latest_public_delta: Option<PublicCurrencyDelta>,
    pending_public_changes: Option<BTreeSet<CurrencyAddress>>,
    validator_transition_proofs: BTreeMap<u64, ValidatorSetTransitionProof>,
    recovery_checkpoint_proof: Option<StateRecoveryCheckpointProof>,
    checkpoint_floor_epoch: u64,
    recovery_checkpoint_floors: BTreeMap<u64, RecoveryCheckpointFloor>,
    validator_safety_ready: bool,
    minimum_signing_validator_set_version: u64,
    pending_validator_safety_recovery: Option<PendingValidatorSafetyRecovery>,
    validator_registry: ValidatorRegistry,
    prepared_tasks: BTreeMap<TaskId, PreparedTask>,
    validator_vote_locks: BTreeMap<(ValidatorId, ConsensusScope), [u8; 32]>,
    bft_local_states: BTreeMap<(ValidatorId, ConsensusScope), BftLocalState>,
}

pub(super) fn encode_snapshot(
    generation: u64,
    contents: SnapshotContents<'_>,
) -> Result<Vec<u8>, PersistenceError> {
    validate_checkpoint_attachment(
        contents.state,
        contents.validator_set,
        contents.public_checkpoint_proof,
        contents.checkpoint_floor_epoch,
    )?;
    contents
        .validator_registry
        .validate_current_set(contents.validator_set)
        .map_err(|_| PersistenceError::ValidatorRegistryMismatch)?;
    validate_governance_evidence(
        contents.state,
        contents.validator_set,
        contents.validator_registry,
        contents.recovery_checkpoint_proof,
        contents.retained_validator_sets,
    )?;
    super::task_handoff::validate_installed(
        contents.state,
        contents.validator_set,
        contents.validator_transition_proofs,
        contents.recovery_checkpoint_proof,
    )?;
    validate_recovery_checkpoint_floors(
        contents.validator_registry,
        contents.recovery_checkpoint_floors,
    )?;
    if contents.minimum_signing_validator_set_version > contents.validator_set.version() {
        return Err(PersistenceError::InvalidSnapshot);
    }
    if contents.validator_safety_ready && contents.pending_validator_safety_recovery.is_some() {
        return Err(PersistenceError::InvalidSnapshot);
    }
    if let Some(pending) = contents.pending_validator_safety_recovery {
        pending.validate_current(contents.validator_set, contents.validator_transition_proofs)?;
    }
    validate_retained_validator_sets(
        contents.validator_set,
        contents.retained_validator_sets,
        contents.validator_registry,
        contents.state,
        contents.prepared_tasks,
        contents.task_receipts,
    )?;
    validate_prepared_finality_proofs(
        contents.validator_set,
        contents.retained_validator_sets,
        contents.prepared_tasks,
    )?;
    validate_vote_lock_registry(contents.validator_registry, contents.validator_vote_locks)?;
    validate_bft_local_state_registry(
        contents.validator_registry,
        contents.validator_set,
        contents.retained_validator_sets,
        contents.prepared_tasks,
        contents.bft_local_states,
    )?;
    validate_active_prepared_vote_lock_membership(
        contents.validator_set,
        contents.retained_validator_sets,
        contents.prepared_tasks,
        contents.validator_vote_locks,
    )?;
    validate_prepared_snapshot_links(
        contents.validator_set,
        contents.retained_validator_sets,
        &contents.state.protocol.task_bindings,
        &contents.state.business.payment_addresses,
        &contents.state.prerequisite.payment_executions,
        contents.prepared_tasks,
        contents.validator_vote_locks,
    )?;
    super::allocation_validation::validate_allocations(
        contents.state,
        contents.validator_set,
        contents.retained_validator_sets,
    )?;
    validate_prepared_plans_against_state(contents.state, contents.prepared_tasks)?;
    super::task_receipts::validate(
        generation,
        contents.state,
        contents.validator_set,
        contents.retained_validator_sets,
        contents.task_receipts,
    )?;
    let payload = encode_payload(&contents)?;
    let payload_len =
        u64::try_from(payload.len()).map_err(|_| PersistenceError::SnapshotTooLarge)?;

    if payload_len > MAX_SNAPSHOT_PAYLOAD_SIZE {
        return Err(PersistenceError::SnapshotTooLarge);
    }

    let mut bytes = Vec::with_capacity(HEADER_SIZE + payload.len() + CHECKSUM_SIZE);
    bytes.extend_from_slice(&SNAPSHOT_MAGIC);
    bytes.extend_from_slice(&SNAPSHOT_VERSION.to_be_bytes());
    bytes.extend_from_slice(&generation.to_be_bytes());
    bytes.extend_from_slice(&payload_len.to_be_bytes());
    bytes.extend_from_slice(&payload);

    let checksum = snapshot_checksum(&bytes);
    bytes.extend_from_slice(&checksum);
    Ok(bytes)
}

pub(super) fn decode_snapshot(bytes: &[u8]) -> Result<PersistedNodeState, PersistenceError> {
    if bytes.len() < HEADER_SIZE + CHECKSUM_SIZE {
        return Err(PersistenceError::InvalidSnapshot);
    }

    if bytes[0..4] != SNAPSHOT_MAGIC {
        return Err(PersistenceError::InvalidSnapshot);
    }

    let version = u32::from_be_bytes(
        bytes[4..8]
            .try_into()
            .map_err(|_| PersistenceError::InvalidSnapshot)?,
    );
    if version != SNAPSHOT_VERSION {
        return Err(PersistenceError::UnsupportedSnapshotVersion(version));
    }

    let generation = u64::from_be_bytes(
        bytes[8..16]
            .try_into()
            .map_err(|_| PersistenceError::InvalidSnapshot)?,
    );
    let payload_len = u64::from_be_bytes(
        bytes[16..24]
            .try_into()
            .map_err(|_| PersistenceError::InvalidSnapshot)?,
    );

    if payload_len > MAX_SNAPSHOT_PAYLOAD_SIZE {
        return Err(PersistenceError::SnapshotTooLarge);
    }

    let payload_len =
        usize::try_from(payload_len).map_err(|_| PersistenceError::SnapshotTooLarge)?;
    let expected_len = HEADER_SIZE
        .checked_add(payload_len)
        .and_then(|len| len.checked_add(CHECKSUM_SIZE))
        .ok_or(PersistenceError::SnapshotTooLarge)?;

    if bytes.len() != expected_len {
        return Err(PersistenceError::InvalidSnapshot);
    }

    let checksum_offset = HEADER_SIZE + payload_len;
    let expected_checksum = snapshot_checksum(&bytes[..checksum_offset]);
    if bytes[checksum_offset..] != expected_checksum {
        return Err(PersistenceError::ChecksumMismatch);
    }

    let decoded = decode_payload(&bytes[HEADER_SIZE..checksum_offset])?;
    validate_checkpoint_attachment(
        &decoded.state,
        &decoded.validator_set,
        decoded.public_checkpoint_proof.as_ref(),
        decoded.checkpoint_floor_epoch,
    )?;
    validate_public_sync_state(
        &decoded.validator_set,
        decoded.public_checkpoint_baseline.as_ref(),
        decoded.latest_public_delta.as_ref(),
        decoded.pending_public_changes.as_ref(),
    )?;
    decoded
        .validator_registry
        .validate_current_set(&decoded.validator_set)
        .map_err(|_| PersistenceError::InvalidSnapshot)?;
    validate_governance_evidence(
        &decoded.state,
        &decoded.validator_set,
        &decoded.validator_registry,
        decoded.recovery_checkpoint_proof.as_ref(),
        &decoded.retained_validator_sets,
    )?;
    super::task_handoff::validate_installed(
        &decoded.state,
        &decoded.validator_set,
        &decoded.validator_transition_proofs,
        decoded.recovery_checkpoint_proof.as_ref(),
    )?;
    validate_recovery_checkpoint_floors(
        &decoded.validator_registry,
        &decoded.recovery_checkpoint_floors,
    )?;
    validate_retained_validator_sets(
        &decoded.validator_set,
        &decoded.retained_validator_sets,
        &decoded.validator_registry,
        &decoded.state,
        &decoded.prepared_tasks,
        &decoded.task_receipts,
    )?;
    super::allocation_validation::validate_allocations(
        &decoded.state,
        &decoded.validator_set,
        &decoded.retained_validator_sets,
    )?;
    super::task_receipts::validate(
        generation,
        &decoded.state,
        &decoded.validator_set,
        &decoded.retained_validator_sets,
        &decoded.task_receipts,
    )?;
    Ok(PersistedNodeState {
        task_receipts: decoded.task_receipts,
        pending_governance: decoded.pending_governance,
        state: decoded.state,
        validator_set: decoded.validator_set,
        validator_registry: decoded.validator_registry,
        retained_validator_sets: decoded.retained_validator_sets,
        public_checkpoint_proof: decoded.public_checkpoint_proof,
        public_checkpoint_baseline: decoded.public_checkpoint_baseline,
        latest_public_delta: decoded.latest_public_delta,
        pending_public_changes: decoded.pending_public_changes,
        validator_transition_proofs: decoded.validator_transition_proofs,
        recovery_checkpoint_proof: decoded.recovery_checkpoint_proof,
        checkpoint_floor_epoch: decoded.checkpoint_floor_epoch,
        recovery_checkpoint_floors: decoded.recovery_checkpoint_floors,
        validator_safety_ready: decoded.validator_safety_ready,
        minimum_signing_validator_set_version: decoded.minimum_signing_validator_set_version,
        pending_validator_safety_recovery: decoded.pending_validator_safety_recovery,
        generation,
        prepared_tasks: decoded.prepared_tasks,
        validator_vote_locks: decoded.validator_vote_locks,
        bft_local_states: decoded.bft_local_states,
    })
}

fn snapshot_checksum(bytes: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(SNAPSHOT_DOMAIN);
    hasher.update(bytes);
    hasher.finalize().into()
}

fn encode_payload(contents: &SnapshotContents<'_>) -> Result<Vec<u8>, PersistenceError> {
    let state = contents.state;
    let validator_set = contents.validator_set;
    let retained_validator_sets = contents.retained_validator_sets;
    let public_checkpoint_proof = contents.public_checkpoint_proof;
    let public_checkpoint_baseline = contents.public_checkpoint_baseline;
    let latest_public_delta = contents.latest_public_delta;
    let pending_public_changes = contents.pending_public_changes;
    let validator_transition_proofs = contents.validator_transition_proofs;
    let recovery_checkpoint_proof = contents.recovery_checkpoint_proof;
    let checkpoint_floor_epoch = contents.checkpoint_floor_epoch;
    let recovery_checkpoint_floors = contents.recovery_checkpoint_floors;
    let validator_safety_ready = contents.validator_safety_ready;
    let minimum_signing_validator_set_version = contents.minimum_signing_validator_set_version;
    let validator_registry = contents.validator_registry;
    let prepared_tasks = contents.prepared_tasks;
    let validator_vote_locks = contents.validator_vote_locks;
    let mut out = Vec::new();

    encode_second_state(&mut out, state)?;
    let proofs = state
        .protocol
        .task_bindings
        .iter()
        .filter_map(|(task_id, binding)| {
            binding
                .allocation_certificate
                .as_ref()
                .map(|certificate| (task_id, binding, certificate))
        })
        .collect::<Vec<_>>();
    push_len(&mut out, proofs.len())?;
    for (task_id, binding, certificate) in proofs {
        push_task_id(&mut out, task_id);
        binding
            .allocation
            .ok_or(PersistenceError::InvalidSnapshot)?;
        let bytes = crate::finality_codec::encode_certificate(certificate)
            .ok_or(PersistenceError::SnapshotTooLarge)?;
        push_len(&mut out, bytes.len())?;
        out.extend_from_slice(&bytes);
    }
    encode_validator_set(&mut out, validator_set)?;
    push_len(&mut out, retained_validator_sets.len())?;
    for retained in retained_validator_sets.values() {
        encode_validator_set(&mut out, retained)?;
    }

    encode_validator_registry(&mut out, validator_registry)?;
    out.extend_from_slice(&checkpoint_floor_epoch.to_be_bytes());
    push_len(&mut out, recovery_checkpoint_floors.len())?;
    for (validator_set_version, floor) in recovery_checkpoint_floors {
        out.extend_from_slice(&validator_set_version.to_be_bytes());
        out.extend_from_slice(&floor.serial.to_be_bytes());
        out.extend_from_slice(&floor.checkpoint_digest);
        out.push(u8::from(floor.certified));
    }

    encode_optional_recovery_proof(&mut out, recovery_checkpoint_proof)?;

    encode_optional_public_proof(&mut out, public_checkpoint_proof)?;
    encode_optional_public_proof(&mut out, public_checkpoint_baseline)?;
    match latest_public_delta {
        Some(delta) => {
            out.push(1);
            let bytes = delta
                .encode_bytes()
                .map_err(|_| PersistenceError::InvalidSnapshot)?;
            push_len(&mut out, bytes.len())?;
            out.extend_from_slice(&bytes);
        }
        None => out.push(0),
    }
    match pending_public_changes {
        Some(changes) => {
            if changes.len() > MAX_PUBLIC_CURRENCY_DELTA_CHANGES {
                return Err(PersistenceError::InvalidSnapshot);
            }
            out.push(1);
            push_len(&mut out, changes.len())?;
            for address in changes {
                out.extend_from_slice(&address.value().to_be_bytes());
            }
        }
        None => out.push(0),
    }
    push_len(&mut out, validator_transition_proofs.len())?;
    for (current_version, proof) in validator_transition_proofs {
        if proof.source().next_validator_set().version() != current_version.saturating_add(1) {
            return Err(PersistenceError::InvalidSnapshot);
        }
        out.extend_from_slice(&current_version.to_be_bytes());
        let bytes = proof
            .encode_bytes()
            .map_err(|_| PersistenceError::InvalidSnapshot)?;
        push_len(&mut out, bytes.len())?;
        out.extend_from_slice(&bytes);
    }

    out.push(u8::from(validator_safety_ready));
    out.extend_from_slice(&minimum_signing_validator_set_version.to_be_bytes());
    encode_optional_pending_safety_recovery(&mut out, contents.pending_validator_safety_recovery)?;
    encode_local_state(
        &mut out,
        prepared_tasks,
        validator_vote_locks,
        contents.bft_local_states,
    )?;

    super::governance::encode_pending(&mut out, contents.pending_governance)?;
    super::task_receipts::encode(&mut out, contents.task_receipts)?;
    Ok(out)
}

pub(crate) fn encode_shared_recovery_state(
    state: &SecondState,
    validator_set: &ValidatorSet,
    validator_registry: &ValidatorRegistry,
    retained_validator_sets: &BTreeMap<u64, ValidatorSet>,
) -> Result<Vec<u8>, PersistenceError> {
    validator_registry
        .validate_current_set(validator_set)
        .map_err(|_| PersistenceError::ValidatorRegistryMismatch)?;

    let mut out = Vec::new();
    encode_second_state(&mut out, state)?;
    encode_validator_set(&mut out, validator_set)?;
    encode_validator_registry(&mut out, validator_registry)?;
    validate_retained_validator_sets(
        validator_set,
        retained_validator_sets,
        validator_registry,
        state,
        &BTreeMap::new(),
        &BTreeMap::new(),
    )?;
    super::allocation_validation::validate_allocations(
        state,
        validator_set,
        retained_validator_sets,
    )?;
    push_len(&mut out, retained_validator_sets.len())?;
    for validators in retained_validator_sets.values() {
        encode_validator_set(&mut out, validators)?;
    }
    Ok(out)
}

pub(super) fn encode_second_state(
    out: &mut Vec<u8>,
    state: &SecondState,
) -> Result<(), PersistenceError> {
    if state
        .business
        .currencies
        .values()
        .any(|currency| currency.address.value() >= state.protocol.next_currency_address)
    {
        return Err(PersistenceError::InvalidSnapshot);
    }

    out.extend_from_slice(&state.protocol.next_currency_address.to_be_bytes());

    push_len(out, state.business.accounts.len())?;
    for account in &state.business.accounts {
        out.extend_from_slice(&account.bytes());
    }

    push_len(out, state.business.payment_addresses.len())?;
    for (address, record) in &state.business.payment_addresses {
        out.extend_from_slice(&address.bytes());
        out.extend_from_slice(&record.account.bytes());
        out.push(match record.status {
            PaymentAddressStatus::Active => 1,
            PaymentAddressStatus::Retiring => 2,
            PaymentAddressStatus::Retired => 3,
        });
    }

    push_len(out, state.business.currencies.len())?;
    for currency in state.business.currencies.values() {
        out.extend_from_slice(&currency.address.value().to_be_bytes());
        out.push(match currency.role {
            CurrencyRole::Circulation => 1,
            CurrencyRole::Reserve => 2,
        });
        match currency.owner {
            Some(owner) => {
                out.push(1);
                out.extend_from_slice(&owner.bytes());
            }
            None => out.push(0),
        }
    }

    push_len(out, state.protocol.task_bindings.len())?;
    for (task_id, binding) in &state.protocol.task_bindings {
        push_task_id(out, task_id);
        out.extend_from_slice(&binding.request_digest);
        match &binding.outcome {
            TaskOutcome::Pending => out.push(0),
            TaskOutcome::Succeeded => out.push(1),
            TaskOutcome::Cancelled(certificate) => {
                out.push(2);
                out.extend_from_slice(&certificate.protocol_version().to_be_bytes());
                out.extend_from_slice(&certificate.validator_set_version().to_be_bytes());
                out.extend_from_slice(&certificate.subject_digest());
            }
        }
        match binding.allocation {
            Some((start, count)) => {
                out.push(1);
                out.extend_from_slice(&start.to_be_bytes());
                out.extend_from_slice(&count.to_be_bytes());
            }
            None => out.push(0),
        }
        match &binding.allocation_task {
            Some(task) => {
                out.push(1);
                let bytes = crate::legal_task_codec::encode_legal_task(task)
                    .map_err(|_| PersistenceError::InvalidSnapshot)?;
                push_len(out, bytes.len())?;
                out.extend_from_slice(&bytes);
            }
            None => out.push(0),
        }
    }

    encode_payments(out, &state.prerequisite.payment_executions)?;
    encode_payments(out, &state.business.payment_history)?;

    match &state.protocol.task_handoff {
        None => out.push(0),
        Some(handoff) => {
            out.push(1);
            let bytes = handoff.encode()?;
            push_len(out, bytes.len())?;
            out.extend_from_slice(&bytes);
        }
    }

    Ok(())
}

fn decode_second_state(decoder: &mut Decoder<'_>) -> Result<SecondState, PersistenceError> {
    decode_second_state_inner(decoder, true)
}

pub(super) fn decode_handoff_business_baseline(
    bytes: &[u8],
) -> Result<SecondState, PersistenceError> {
    let mut decoder = Decoder::new(bytes);
    let state = decode_second_state_inner(&mut decoder, false)?;
    decoder.finish()?;
    if !state.prerequisite.payment_executions.is_empty()
        || state.protocol.task_bindings.values().any(|binding| {
            binding.allocation_task.is_some()
                || (!binding.outcome.is_terminal() && binding.allocation.is_none())
        })
    {
        return Err(PersistenceError::InvalidSnapshot);
    }
    let mut canonical = Vec::new();
    encode_second_state(&mut canonical, &state)?;
    if canonical != bytes || state.same_persisted_state(&SecondState::genesis([], 1)) {
        return Err(PersistenceError::InvalidSnapshot);
    }
    Ok(state)
}

fn decode_second_state_inner(
    decoder: &mut Decoder<'_>,
    allow_handoff: bool,
) -> Result<SecondState, PersistenceError> {
    let next_currency_address = decoder.read_u64()?;

    let account_count = decoder.read_len()?;
    if account_count > decoder.remaining() / 32 {
        return Err(PersistenceError::InvalidSnapshot);
    }
    let mut accounts = BTreeSet::new();
    for _ in 0..account_count {
        let account = AccountAddress::from_bytes(decoder.read_array_32()?);
        if !accounts.insert(account) {
            return Err(PersistenceError::InvalidSnapshot);
        }
    }

    let payment_address_count = decoder.read_len()?;
    const PAYMENT_ADDRESS_ENCODED_SIZE: usize = 32 + 32 + 1;
    if payment_address_count > decoder.remaining() / PAYMENT_ADDRESS_ENCODED_SIZE {
        return Err(PersistenceError::InvalidSnapshot);
    }
    let mut payment_addresses = BTreeMap::new();
    for _ in 0..payment_address_count {
        let address = PaymentAddress::from_bytes(decoder.read_array_32()?);
        let account = AccountAddress::from_bytes(decoder.read_array_32()?);
        if !accounts.contains(&account) {
            return Err(PersistenceError::InvalidSnapshot);
        }
        let status = match decoder.read_u8()? {
            1 => PaymentAddressStatus::Active,
            2 => PaymentAddressStatus::Retiring,
            3 => PaymentAddressStatus::Retired,
            _ => return Err(PersistenceError::InvalidSnapshot),
        };
        if payment_addresses
            .insert(address, PaymentAddressRecord { account, status })
            .is_some()
        {
            return Err(PersistenceError::InvalidSnapshot);
        }
    }

    let currency_count = decoder.read_len()?;
    let mut currencies = BTreeMap::new();
    for _ in 0..currency_count {
        let address = CurrencyAddress::new(decoder.read_u64()?);
        if address.value() >= next_currency_address {
            return Err(PersistenceError::InvalidSnapshot);
        }

        let role = match decoder.read_u8()? {
            1 => CurrencyRole::Circulation,
            2 => CurrencyRole::Reserve,
            _ => return Err(PersistenceError::InvalidSnapshot),
        };

        let owner = match decoder.read_u8()? {
            0 => None,
            1 => {
                let owner = AccountAddress::from_bytes(decoder.read_array_32()?);
                if !accounts.contains(&owner) {
                    return Err(PersistenceError::InvalidSnapshot);
                }
                Some(owner)
            }
            _ => return Err(PersistenceError::InvalidSnapshot),
        };

        if role == CurrencyRole::Reserve && owner.is_some() {
            return Err(PersistenceError::InvalidSnapshot);
        }

        if currencies
            .insert(
                address,
                Currency {
                    address,
                    role,
                    owner,
                },
            )
            .is_some()
        {
            return Err(PersistenceError::InvalidSnapshot);
        }
    }

    let task_count = decoder.read_len()?;
    const MIN_TASK_BINDING_SIZE: usize = 1 + 1 + 32 + 1;
    if task_count > decoder.remaining() / MIN_TASK_BINDING_SIZE {
        return Err(PersistenceError::InvalidSnapshot);
    }
    let mut task_bindings = BTreeMap::new();
    for _ in 0..task_count {
        let task_id = decoder.read_task_id()?;
        let request_digest = decoder.read_array_32()?;
        let outcome = match decoder.read_u8()? {
            0 => TaskOutcome::Pending,
            1 => TaskOutcome::Succeeded,
            2 => TaskOutcome::Cancelled(crate::FinalityStatement::new(
                u32::from_be_bytes(decoder.read_exact(4)?.try_into().unwrap()),
                decoder.read_u64()?,
                decoder.read_array_32()?,
            )),
            _ => return Err(PersistenceError::InvalidSnapshot),
        };
        let allocation = match decoder.read_u8()? {
            0 => None,
            1 => {
                let start = decoder.read_u64()?;
                let count = decoder.read_u64()?;
                if count == 0
                    || start
                        .checked_add(count)
                        .is_none_or(|end| end > next_currency_address)
                {
                    return Err(PersistenceError::InvalidSnapshot);
                }
                Some((start, count))
            }
            _ => return Err(PersistenceError::InvalidSnapshot),
        };
        let allocation_task = match decoder.read_u8()? {
            0 => None,
            1 => {
                let length = decoder.read_len()?;
                let task = crate::legal_task_codec::decode_legal_task(decoder.read_exact(length)?)
                    .ok_or(PersistenceError::InvalidSnapshot)?;
                if task.payload().task_id() != task_id
                    || task
                        .request_digest()
                        .map_err(|_| PersistenceError::InvalidSnapshot)?
                        != request_digest
                    || outcome.is_terminal()
                {
                    return Err(PersistenceError::InvalidSnapshot);
                }
                Some(task)
            }
            _ => return Err(PersistenceError::InvalidSnapshot),
        };

        let allocation_certificate = None;
        if task_bindings
            .insert(
                task_id,
                TaskBinding {
                    request_digest,
                    outcome,
                    allocation,
                    allocation_task,
                    allocation_certificate,
                },
            )
            .is_some()
        {
            return Err(PersistenceError::InvalidSnapshot);
        }
    }

    let payment_executions = decode_payments(decoder)?;
    let payment_history = decode_payments(decoder)?;
    for (claim_id, execution) in &payment_history {
        if !task_bindings
            .get(claim_id.task_id())
            .is_some_and(|binding| binding.outcome == TaskOutcome::Succeeded)
            || !payment_addresses.contains_key(&execution.source)
            || !payment_addresses.contains_key(&execution.destination)
            || payment_executions.contains_key(claim_id)
        {
            return Err(PersistenceError::InvalidSnapshot);
        }
    }

    for claim_id in payment_executions.keys() {
        let binding = task_bindings
            .get(claim_id.task_id())
            .ok_or(PersistenceError::InvalidSnapshot)?;
        if binding.outcome.is_terminal() {
            return Err(PersistenceError::InvalidSnapshot);
        }
    }

    Ok(SecondState {
        protocol: ProtocolState {
            next_currency_address,
            task_bindings,
            task_handoff: match decoder.read_u8()? {
                0 => None,
                1 if allow_handoff => {
                    let length = decoder.read_len()?;
                    Some(std::sync::Arc::new(super::TaskHandoff::decode(
                        decoder.read_exact(length)?,
                    )?))
                }
                _ => return Err(PersistenceError::InvalidSnapshot),
            },
        },
        prerequisite: PrerequisiteState { payment_executions },
        business: BusinessState {
            accounts,
            payment_addresses,
            currencies,
            payment_history,
        },
    })
}

fn encode_payments(
    out: &mut Vec<u8>,
    payments: &BTreeMap<OperationClaimId, PaymentExecution>,
) -> Result<(), PersistenceError> {
    push_len(out, payments.len())?;
    for (claim_id, execution) in payments {
        push_task_id(out, claim_id.task_id());
        out.extend_from_slice(&claim_id.operation_index().to_be_bytes());
        out.extend_from_slice(&execution.source.bytes());
        out.extend_from_slice(&execution.destination.bytes());
        out.extend_from_slice(&execution.amount.to_be_bytes());
    }
    Ok(())
}

fn decode_payments(
    decoder: &mut Decoder<'_>,
) -> Result<BTreeMap<OperationClaimId, PaymentExecution>, PersistenceError> {
    let count = decoder.read_len()?;
    const MIN_PAYMENT_ENCODED_SIZE: usize = 1 + 1 + 8 + 32 + 32 + 8;
    if count > decoder.remaining() / MIN_PAYMENT_ENCODED_SIZE {
        return Err(PersistenceError::InvalidSnapshot);
    }
    let mut payments = BTreeMap::new();
    for _ in 0..count {
        let claim_id = OperationClaimId::new(decoder.read_task_id()?, decoder.read_u64()?);
        let execution = PaymentExecution {
            source: PaymentAddress::from_bytes(decoder.read_array_32()?),
            destination: PaymentAddress::from_bytes(decoder.read_array_32()?),
            amount: decoder.read_u64()?,
        };
        if execution.amount == 0 || payments.insert(claim_id, execution).is_some() {
            return Err(PersistenceError::InvalidSnapshot);
        }
    }
    Ok(payments)
}

pub(crate) fn decode_shared_recovery_state(
    bytes: &[u8],
) -> Result<
    (
        SecondState,
        ValidatorSet,
        ValidatorRegistry,
        BTreeMap<u64, ValidatorSet>,
    ),
    PersistenceError,
> {
    let mut decoder = Decoder::new(bytes);
    let state = decode_second_state(&mut decoder)?;
    let validator_set = decode_validator_set(&mut decoder)?;
    let validator_registry = decode_validator_registry(&mut decoder)?;
    let count = decoder.read_len()?;
    if count > decoder.remaining() / 12 {
        return Err(PersistenceError::InvalidSnapshot);
    }
    let mut retained = BTreeMap::new();
    for _ in 0..count {
        let validators = decode_validator_set(&mut decoder)?;
        if retained.insert(validators.version(), validators).is_some() {
            return Err(PersistenceError::InvalidSnapshot);
        }
    }
    decoder.finish()?;
    validate_retained_validator_sets(
        &validator_set,
        &retained,
        &validator_registry,
        &state,
        &BTreeMap::new(),
        &BTreeMap::new(),
    )?;
    super::allocation_validation::validate_allocations(&state, &validator_set, &retained)?;
    validator_registry
        .validate_current_set(&validator_set)
        .map_err(|_| PersistenceError::ValidatorRegistryMismatch)?;
    Ok((state, validator_set, validator_registry, retained))
}

fn decode_payload(payload: &[u8]) -> Result<DecodedSnapshotPayload, PersistenceError> {
    let mut decoder = Decoder::new(payload);

    let mut state = decode_second_state(&mut decoder)?;
    for _ in 0..decoder.read_len()? {
        let task_id = decoder.read_task_id()?;
        let length = decoder.read_len()?;
        let binding = state
            .protocol
            .task_bindings
            .get_mut(&task_id)
            .ok_or(PersistenceError::InvalidSnapshot)?;
        if binding.allocation.is_none() || binding.allocation_certificate.is_some() {
            return Err(PersistenceError::InvalidSnapshot);
        }
        binding.allocation_certificate = Some(
            crate::finality_codec::decode_certificate(decoder.read_exact(length)?)
                .ok_or(PersistenceError::InvalidSnapshot)?,
        );
    }

    let validator_set = decode_validator_set(&mut decoder)?;
    let retained_count = decoder.read_len()?;
    const MIN_VALIDATOR_SET_SIZE: usize = 8 + 8 + 8 + 32 * 3;
    if retained_count > decoder.remaining() / MIN_VALIDATOR_SET_SIZE {
        return Err(PersistenceError::InvalidSnapshot);
    }
    let mut retained_validator_sets = BTreeMap::new();
    for _ in 0..retained_count {
        let retained = decode_validator_set(&mut decoder)?;
        if retained_validator_sets
            .insert(retained.version(), retained)
            .is_some()
        {
            return Err(PersistenceError::InvalidSnapshot);
        }
    }

    let validator_registry = decode_validator_registry(&mut decoder)?;
    let checkpoint_floor_epoch = decoder.read_u64()?;
    let recovery_floor_count = decoder.read_len()?;
    const RECOVERY_FLOOR_ENCODED_SIZE: usize = 8 + 8 + 32 + 1;
    if recovery_floor_count > decoder.remaining() / RECOVERY_FLOOR_ENCODED_SIZE {
        return Err(PersistenceError::InvalidSnapshot);
    }
    let mut recovery_checkpoint_floors = BTreeMap::new();
    for _ in 0..recovery_floor_count {
        let validator_set_version = decoder.read_u64()?;
        let serial = decoder.read_u64()?;
        let checkpoint_digest = decoder.read_array_32()?;
        let certified = match decoder.read_u8()? {
            0 => false,
            1 => true,
            _ => return Err(PersistenceError::InvalidSnapshot),
        };
        let floor = RecoveryCheckpointFloor {
            serial,
            checkpoint_digest,
            certified,
        };
        if recovery_checkpoint_floors
            .insert(validator_set_version, floor)
            .is_some()
        {
            return Err(PersistenceError::InvalidSnapshot);
        }
    }

    let recovery_checkpoint_proof = decode_optional_recovery_proof(&mut decoder)?;

    let public_checkpoint_proof = decode_optional_public_proof(&mut decoder)?;
    let public_checkpoint_baseline = decode_optional_public_proof(&mut decoder)?;
    let latest_public_delta = match decoder.read_u8()? {
        0 => None,
        1 => {
            let len = decoder.read_len()?;
            Some(
                PublicCurrencyDelta::decode_bytes(decoder.read_exact(len)?)
                    .map_err(|_| PersistenceError::InvalidSnapshot)?,
            )
        }
        _ => return Err(PersistenceError::InvalidSnapshot),
    };
    let pending_public_changes = match decoder.read_u8()? {
        0 => None,
        1 => {
            let count = decoder.read_len()?;
            if count > MAX_PUBLIC_CURRENCY_DELTA_CHANGES || count > decoder.remaining() / 8 {
                return Err(PersistenceError::InvalidSnapshot);
            }
            let mut changes = BTreeSet::new();
            for _ in 0..count {
                if !changes.insert(CurrencyAddress::new(decoder.read_u64()?)) {
                    return Err(PersistenceError::InvalidSnapshot);
                }
            }
            Some(changes)
        }
        _ => return Err(PersistenceError::InvalidSnapshot),
    };
    let transition_count = decoder.read_len()?;
    let mut validator_transition_proofs = BTreeMap::new();
    for _ in 0..transition_count {
        let current_version = decoder.read_u64()?;
        let len = decoder.read_len()?;
        let proof = ValidatorSetTransitionProof::decode_bytes(decoder.read_exact(len)?)
            .map_err(|_| PersistenceError::InvalidSnapshot)?;
        if proof.source().next_validator_set().version() != current_version.saturating_add(1)
            || validator_transition_proofs
                .insert(current_version, proof)
                .is_some()
        {
            return Err(PersistenceError::InvalidSnapshot);
        }
    }

    let validator_safety_ready = match decoder.read_u8()? {
        0 => false,
        1 => true,
        _ => return Err(PersistenceError::InvalidSnapshot),
    };
    let minimum_signing_validator_set_version = decoder.read_u64()?;
    if minimum_signing_validator_set_version > validator_set.version() {
        return Err(PersistenceError::InvalidSnapshot);
    }
    let pending_validator_safety_recovery = decode_optional_pending_safety_recovery(&mut decoder)?;
    if validator_safety_ready && pending_validator_safety_recovery.is_some() {
        return Err(PersistenceError::InvalidSnapshot);
    }
    if let Some(pending) = pending_validator_safety_recovery.as_ref() {
        pending
            .validate_current(&validator_set, &validator_transition_proofs)
            .map_err(|_| PersistenceError::InvalidSnapshot)?;
    }
    let (prepared_tasks, validator_vote_locks, bft_local_states) =
        decode_local_state(&mut decoder)?;

    let pending_governance = super::governance::decode_pending(
        &mut decoder,
        &validator_set,
        &validator_registry,
        state.next_currency_address(),
    )?;
    let task_receipts = super::task_receipts::decode(&mut decoder)?;
    decoder.finish()?;

    validate_vote_lock_registry(&validator_registry, &validator_vote_locks)?;
    validate_bft_local_state_registry(
        &validator_registry,
        &validator_set,
        &retained_validator_sets,
        &prepared_tasks,
        &bft_local_states,
    )?;
    validate_active_prepared_vote_lock_membership(
        &validator_set,
        &retained_validator_sets,
        &prepared_tasks,
        &validator_vote_locks,
    )?;
    validate_prepared_finality_proofs(&validator_set, &retained_validator_sets, &prepared_tasks)?;
    validate_prepared_snapshot_links(
        &validator_set,
        &retained_validator_sets,
        &state.protocol.task_bindings,
        &state.business.payment_addresses,
        &state.prerequisite.payment_executions,
        &prepared_tasks,
        &validator_vote_locks,
    )?;
    validate_prepared_plans_against_state(&state, &prepared_tasks)?;

    Ok(DecodedSnapshotPayload {
        task_receipts,
        pending_governance,
        state,
        validator_set,
        retained_validator_sets,
        public_checkpoint_proof,
        public_checkpoint_baseline,
        latest_public_delta,
        pending_public_changes,
        validator_transition_proofs,
        recovery_checkpoint_proof,
        checkpoint_floor_epoch,
        recovery_checkpoint_floors,
        validator_safety_ready,
        minimum_signing_validator_set_version,
        pending_validator_safety_recovery,
        validator_registry,
        prepared_tasks,
        validator_vote_locks,
        bft_local_states,
    })
}

fn encode_optional_public_proof(
    out: &mut Vec<u8>,
    proof: Option<&PublicCurrencyCheckpointProof>,
) -> Result<(), PersistenceError> {
    match proof {
        Some(proof) => {
            out.push(1);
            let bytes = proof
                .encode_bytes()
                .map_err(|_| PersistenceError::InvalidSnapshot)?;
            push_len(out, bytes.len())?;
            out.extend_from_slice(&bytes);
        }
        None => out.push(0),
    }
    Ok(())
}

fn decode_optional_public_proof(
    decoder: &mut Decoder<'_>,
) -> Result<Option<PublicCurrencyCheckpointProof>, PersistenceError> {
    match decoder.read_u8()? {
        0 => Ok(None),
        1 => {
            let len = decoder.read_len()?;
            PublicCurrencyCheckpointProof::decode_bytes(decoder.read_exact(len)?)
                .map(Some)
                .map_err(|_| PersistenceError::InvalidSnapshot)
        }
        _ => Err(PersistenceError::InvalidSnapshot),
    }
}

fn validate_public_sync_state(
    validator_set: &ValidatorSet,
    baseline: Option<&PublicCurrencyCheckpointProof>,
    latest_delta: Option<&PublicCurrencyDelta>,
    pending_changes: Option<&BTreeSet<CurrencyAddress>>,
) -> Result<(), PersistenceError> {
    if pending_changes.is_some_and(|changes| changes.len() > MAX_PUBLIC_CURRENCY_DELTA_CHANGES) {
        return Err(PersistenceError::InvalidSnapshot);
    }
    let certified_baseline = baseline
        .map(|proof| {
            proof
                .clone()
                .verify_checkpoint(validator_set)
                .map_err(PersistenceError::PublicCheckpoint)
        })
        .transpose()?;
    match (certified_baseline.as_ref(), latest_delta) {
        (Some(checkpoint), Some(delta)) => {
            if delta.to_epoch() != checkpoint.checkpoint().epoch()
                || delta.summary() != checkpoint.checkpoint().summary()
            {
                return Err(PersistenceError::InvalidSnapshot);
            }
        }
        (None, Some(_)) => return Err(PersistenceError::InvalidSnapshot),
        _ => {}
    }
    Ok(())
}

fn encode_optional_recovery_proof(
    out: &mut Vec<u8>,
    proof: Option<&StateRecoveryCheckpointProof>,
) -> Result<(), PersistenceError> {
    match proof {
        Some(proof) => {
            out.push(1);
            let bytes = proof
                .encode_bytes()
                .map_err(|_| PersistenceError::InvalidSnapshot)?;
            push_len(out, bytes.len())?;
            out.extend_from_slice(&bytes);
        }
        None => out.push(0),
    }
    Ok(())
}

fn decode_optional_recovery_proof(
    decoder: &mut Decoder<'_>,
) -> Result<Option<StateRecoveryCheckpointProof>, PersistenceError> {
    match decoder.read_u8()? {
        0 => Ok(None),
        1 => {
            let len = decoder.read_len()?;
            let bytes = decoder.read_exact(len)?;
            StateRecoveryCheckpointProof::decode_bytes(bytes)
                .map(Some)
                .map_err(|_| PersistenceError::InvalidSnapshot)
        }
        _ => Err(PersistenceError::InvalidSnapshot),
    }
}

fn validate_governance_evidence(
    state: &SecondState,
    validator_set: &ValidatorSet,
    validator_registry: &ValidatorRegistry,
    recovery_proof: Option<&StateRecoveryCheckpointProof>,
    retained_validator_sets: &BTreeMap<u64, ValidatorSet>,
) -> Result<(), PersistenceError> {
    if let Some(proof) = recovery_proof {
        let certified = proof
            .clone()
            .verify_checkpoint(validator_set)
            .map_err(PersistenceError::RecoveryCheckpointFinality)?;
        let payload = StateRecoveryPayload::from_shared_parts(
            state,
            validator_set,
            validator_registry,
            retained_validator_sets,
        )?;
        certified.verify_payload(&payload, validator_set)?;
    }
    Ok(())
}

fn validate_recovery_checkpoint_floors(
    validator_registry: &ValidatorRegistry,
    recovery_checkpoint_floors: &BTreeMap<u64, RecoveryCheckpointFloor>,
) -> Result<(), PersistenceError> {
    if recovery_checkpoint_floors.iter().any(|(version, floor)| {
        *version > validator_registry.active_validator_set_version() || floor.serial == 0
    }) {
        return Err(PersistenceError::InvalidSnapshot);
    }
    Ok(())
}

fn validate_checkpoint_attachment(
    state: &SecondState,
    validator_set: &ValidatorSet,
    public_checkpoint_proof: Option<&PublicCurrencyCheckpointProof>,
    checkpoint_floor_epoch: u64,
) -> Result<(), PersistenceError> {
    let Some(proof) = public_checkpoint_proof else {
        return Ok(());
    };

    if proof.checkpoint().summary() != &state.public_currency_summary() {
        return Err(PersistenceError::CheckpointDoesNotMatchState);
    }

    if proof.validator_set_version() != validator_set.version() {
        return Err(PersistenceError::CheckpointValidatorSetMismatch {
            expected: validator_set.version(),
            actual: proof.validator_set_version(),
        });
    }

    let actual_epoch = proof.checkpoint().epoch();
    if actual_epoch < checkpoint_floor_epoch {
        return Err(PersistenceError::StaleCheckpointEpoch {
            minimum: checkpoint_floor_epoch,
            actual: actual_epoch,
        });
    }

    Ok(())
}

pub(super) fn push_len(out: &mut Vec<u8>, len: usize) -> Result<(), PersistenceError> {
    let len = u64::try_from(len).map_err(|_| PersistenceError::SnapshotTooLarge)?;
    out.extend_from_slice(&len.to_be_bytes());
    Ok(())
}

pub(super) fn push_task_id(out: &mut Vec<u8>, task_id: &TaskId) {
    out.push(task_id.len() as u8);
    out.extend_from_slice(task_id.as_bytes());
}

pub(super) struct Decoder<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Decoder<'a> {
    pub(super) fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    pub(super) fn read_u8(&mut self) -> Result<u8, PersistenceError> {
        Ok(self.read_exact(1)?[0])
    }

    pub(super) fn read_u64(&mut self) -> Result<u64, PersistenceError> {
        Ok(u64::from_be_bytes(
            self.read_exact(8)?
                .try_into()
                .map_err(|_| PersistenceError::InvalidSnapshot)?,
        ))
    }

    pub(super) fn read_task_id(&mut self) -> Result<TaskId, PersistenceError> {
        let len = usize::from(self.read_u8()?);
        if !(1..=128).contains(&len) {
            return Err(PersistenceError::InvalidSnapshot);
        }
        TaskId::from_ascii_bytes(self.read_exact(len)?)
            .map_err(|_| PersistenceError::InvalidSnapshot)
    }

    pub(super) fn read_array_32(&mut self) -> Result<[u8; 32], PersistenceError> {
        self.read_exact(32)?
            .try_into()
            .map_err(|_| PersistenceError::InvalidSnapshot)
    }

    pub(super) fn read_len(&mut self) -> Result<usize, PersistenceError> {
        usize::try_from(self.read_u64()?).map_err(|_| PersistenceError::InvalidSnapshot)
    }

    pub(super) fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.offset)
    }

    pub(super) fn read_exact(&mut self, len: usize) -> Result<&'a [u8], PersistenceError> {
        let end = self
            .offset
            .checked_add(len)
            .ok_or(PersistenceError::InvalidSnapshot)?;

        if end > self.bytes.len() {
            return Err(PersistenceError::InvalidSnapshot);
        }

        let slice = &self.bytes[self.offset..end];
        self.offset = end;
        Ok(slice)
    }

    pub(super) fn finish(self) -> Result<(), PersistenceError> {
        if self.offset == self.bytes.len() {
            Ok(())
        } else {
            Err(PersistenceError::InvalidSnapshot)
        }
    }
}
