mod codec;
mod store;

pub use store::StateStore;

use crate::{SecondState, ValidatorSet};

#[derive(Clone)]
pub struct PersistedNodeState {
    pub state: SecondState,
    pub validator_set: ValidatorSet,
    pub generation: u64,
}
