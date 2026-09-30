mod codec;
#[cfg(test)]
mod codec_tests;
mod local_codec;
mod prepared_validation;
mod slot;
mod store;
mod validator_codec;

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
    pub public_checkpoint_proof: Option<PublicCurrencyCheckpointProof>,
    pub checkpoint_floor_epoch: u64,
    pub generation: u64,
    pub(crate) prepared_tasks: BTreeMap<TaskId, PreparedTask>,
    pub(crate) validator_vote_locks: BTreeMap<(ValidatorId, FinalityScope), [u8; 32]>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum VoteLockStatus {
    Inserted,
    AlreadyLocked,
    Conflict([u8; 32]),
}
