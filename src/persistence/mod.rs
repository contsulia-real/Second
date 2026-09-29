mod codec;
mod store;

pub use store::StateStore;

use crate::{PublicCurrencyCheckpointProof, SecondState, ValidatorSet};

#[derive(Clone)]
pub struct PersistedNodeState {
    pub state: SecondState,
    pub validator_set: ValidatorSet,
    pub public_checkpoint_proof: Option<PublicCurrencyCheckpointProof>,
    pub generation: u64,
}
