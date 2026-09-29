use std::collections::BTreeSet;

use crate::{ValidatorId, ValidatorSetError};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidatorSet {
    version: u64,
    validators: BTreeSet<ValidatorId>,
}

impl ValidatorSet {
    pub fn new<I>(version: u64, validators: I) -> Result<Self, ValidatorSetError>
    where
        I: IntoIterator<Item = ValidatorId>,
    {
        let mut set = BTreeSet::new();
        for validator in validators {
            if !set.insert(validator) {
                return Err(ValidatorSetError::DuplicateValidator);
            }
        }

        if set.is_empty() {
            return Err(ValidatorSetError::EmptySet);
        }

        Ok(Self {
            version,
            validators: set,
        })
    }

    pub const fn version(&self) -> u64 {
        self.version
    }

    pub fn len(&self) -> usize {
        self.validators.len()
    }

    pub fn is_empty(&self) -> bool {
        self.validators.is_empty()
    }

    pub fn quorum_threshold(&self) -> usize {
        let n = self.validators.len();
        n - (n - 1) / 3
    }

    pub fn contains(&self, validator: ValidatorId) -> bool {
        self.validators.contains(&validator)
    }

    pub fn has_quorum<I>(&self, votes: I) -> bool
    where
        I: IntoIterator<Item = ValidatorId>,
    {
        let unique_valid_votes = votes
            .into_iter()
            .filter(|validator| self.validators.contains(validator))
            .collect::<BTreeSet<_>>();

        unique_valid_votes.len() >= self.quorum_threshold()
    }
}
