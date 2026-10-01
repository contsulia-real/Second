mod codec;
#[cfg(test)]
mod codec_tests;
mod local_codec;
mod slot;
mod snapshot_validation;
mod store;
mod validator_codec;

pub(crate) use codec::{decode_shared_recovery_state, encode_shared_recovery_state};
pub use store::StateStore;

use std::collections::BTreeMap;

use crate::prepared_plan::PreparedTask;
use crate::validator_signer::FinalityScope;
use crate::{
    PublicCurrencyCheckpointProof, SecondState, TaskId, ValidatorId, ValidatorRegistry,
    ValidatorSet,
};

#[derive(Clone)]
pub struct PersistedNodeState {
    pub state: SecondState,
    pub validator_set: ValidatorSet,
    pub validator_registry: ValidatorRegistry,
    pub retained_validator_sets: BTreeMap<u64, ValidatorSet>,
    pub public_checkpoint_proof: Option<PublicCurrencyCheckpointProof>,
    pub checkpoint_floor_epoch: u64,
    pub(crate) recovery_checkpoint_floors: BTreeMap<u64, RecoveryCheckpointFloor>,
    pub validator_safety_ready: bool,
    pub minimum_signing_validator_set_version: u64,
    pub generation: u64,
    pub(crate) prepared_tasks: BTreeMap<TaskId, PreparedTask>,
    pub(crate) validator_vote_locks: BTreeMap<(ValidatorId, FinalityScope), [u8; 32]>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RecoveryCheckpointFloor {
    pub(crate) serial: u64,
    pub(crate) checkpoint_digest: [u8; 32],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum VoteLockStatus {
    Inserted,
    AlreadyLocked,
    Conflict([u8; 32]),
}
