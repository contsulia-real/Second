mod bft_store;
mod codec;
#[cfg(test)]
mod codec_tests;
mod local_codec;
mod public_store;
mod safety_recovery;
mod slot;
mod snapshot_validation;
mod store;
mod validator_codec;

pub(crate) use codec::{decode_shared_recovery_state, encode_shared_recovery_state};
pub use public_store::{PersistedPublicNodeState, PublicStateStore};
pub(crate) use safety_recovery::PendingValidatorSafetyRecovery;
pub use store::StateStore;

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
