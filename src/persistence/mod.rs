mod allocation_validation;
mod bft_store;
mod blob_store;
pub use blob_store::DurableBlobStore;
#[cfg(test)]
mod certified_choice_tests;
mod codec;
#[cfg(test)]
mod codec_tests;
mod commit;
mod contention;
pub(crate) use contention::handoff_evidence_digests;
pub(crate) use contention::{contender_index, contenders_for_plan, has_commit_evidence};
mod governance;
#[cfg(test)]
mod handoff_baseline_tests;
mod local_codec;
mod public_store;
mod read_cache;
pub(crate) mod recovery_payload_codec;
mod safety_recovery;
pub(crate) mod slot;
mod snapshot_validation;
pub(crate) use snapshot_validation::resolve_validator_set;
mod store;
mod store_abort;
mod store_allocation;
mod store_finality;
mod store_handoff_import;
mod store_prepared;
mod store_public_checkpoint;
mod store_recovery;
mod store_transition;
mod task_receipts;
pub(crate) use task_receipts::TaskReceipts;
pub(crate) mod task_handoff;
mod validator_codec;

pub(crate) use codec::{decode_shared_recovery_state, encode_shared_recovery_state};
pub(crate) use governance::PendingGovernance;
pub use public_store::{PersistedPublicNodeState, PublicStateStore};
pub(crate) use safety_recovery::PendingValidatorSafetyRecovery;
pub use store::StateStore;
pub(crate) use task_handoff::TaskHandoff;
pub(crate) fn validate_transition_vote(
    snapshot: &PersistedNodeState,
    target: &crate::runtime_consensus_target::ValidatorConsensusTarget,
) -> Result<(), crate::PersistenceError> {
    bft_store::governance_subject(snapshot, target).map(|_| ())
}

use std::collections::{BTreeMap, BTreeSet};

use crate::ConsensusScope;
use crate::prepared_plan::PreparedTask;
use crate::{
    BftLocalState, CurrencyAddress, PublicCurrencyCheckpointProof, PublicCurrencyDelta,
    SecondState, StateRecoveryCheckpointProof, TaskId, ValidatorId, ValidatorRegistry,
    ValidatorSet, ValidatorSetTransitionProof,
};

#[derive(Clone)]
pub struct PersistedNodeState {
    pub(crate) task_receipts: TaskReceipts,
    pub state: SecondState,
    pub validator_set: ValidatorSet,
    pub validator_registry: ValidatorRegistry,
    pub retained_validator_sets: BTreeMap<u64, ValidatorSet>,
    pub public_checkpoint_proof: Option<PublicCurrencyCheckpointProof>,
    pub public_checkpoint_baseline: Option<PublicCurrencyCheckpointProof>,
    pub latest_public_delta: Option<PublicCurrencyDelta>,
    pub(crate) pending_public_changes: Option<BTreeSet<CurrencyAddress>>,
    pub(crate) validator_transition_proofs: BTreeMap<u64, ValidatorSetTransitionProof>,
    pub recovery_checkpoint_proof: Option<StateRecoveryCheckpointProof>,
    pub checkpoint_floor_epoch: u64,
    pub(crate) recovery_checkpoint_floors: BTreeMap<u64, RecoveryCheckpointFloor>,
    pub validator_safety_ready: bool,
    pub minimum_signing_validator_set_version: u64,
    pub(crate) pending_validator_safety_recovery: Option<PendingValidatorSafetyRecovery>,
    pub(crate) pending_governance: BTreeMap<[u8; 32], PendingGovernance>,
    pub generation: u64,
    pub(crate) prepared_tasks: BTreeMap<TaskId, PreparedTask>,
    pub(crate) validator_vote_locks: BTreeMap<(ValidatorId, ConsensusScope), [u8; 32]>,
    pub(crate) bft_local_states: BTreeMap<(ValidatorId, ConsensusScope), BftLocalState>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RecoveryCheckpointFloor {
    pub(crate) serial: u64,
    pub(crate) checkpoint_digest: [u8; 32],
    pub(crate) certified: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum VoteLockStatus {
    Inserted,
    AlreadyLocked,
    Conflict([u8; 32]),
}
