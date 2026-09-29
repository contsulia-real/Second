use std::collections::{BTreeMap, BTreeSet};

use ed25519_dalek::VerifyingKey;

use crate::{ValidatorId, ValidatorSetError};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidatorCredential {
    id: ValidatorId,
    consensus_public_key: [u8; 32],
}

impl ValidatorCredential {
    pub fn new(id: ValidatorId, consensus_public_key: [u8; 32]) -> Result<Self, ValidatorSetError> {
        VerifyingKey::from_bytes(&consensus_public_key)
            .map_err(|_| ValidatorSetError::InvalidConsensusKey(id))?;

        Ok(Self {
            id,
            consensus_public_key,
        })
    }

    pub const fn id(&self) -> ValidatorId {
        self.id
    }

    pub const fn consensus_public_key(&self) -> [u8; 32] {
        self.consensus_public_key
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidatorSet {
    version: u64,
    validators: BTreeMap<ValidatorId, ValidatorCredential>,
}

impl ValidatorSet {
    pub fn new<I>(version: u64, validators: I) -> Result<Self, ValidatorSetError>
    where
        I: IntoIterator<Item = ValidatorCredential>,
    {
        let mut by_id = BTreeMap::new();
        let mut consensus_keys = BTreeSet::new();

        for validator in validators {
            if by_id.contains_key(&validator.id()) {
                return Err(ValidatorSetError::DuplicateValidator);
            }
            if !consensus_keys.insert(validator.consensus_public_key()) {
                return Err(ValidatorSetError::DuplicateConsensusKey);
            }
            by_id.insert(validator.id(), validator);
        }

        if by_id.is_empty() {
            return Err(ValidatorSetError::EmptySet);
        }

        Ok(Self {
            version,
            validators: by_id,
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
        self.validators.contains_key(&validator)
    }

    pub fn has_quorum<I>(&self, votes: I) -> bool
    where
        I: IntoIterator<Item = ValidatorId>,
    {
        let unique_valid_votes = votes
            .into_iter()
            .filter(|validator| self.validators.contains_key(validator))
            .collect::<BTreeSet<_>>();

        unique_valid_votes.len() >= self.quorum_threshold()
    }

    pub(crate) fn credential(&self, validator: ValidatorId) -> Option<&ValidatorCredential> {
        self.validators.get(&validator)
    }

    pub(crate) fn credentials(&self) -> impl Iterator<Item = &ValidatorCredential> {
        self.validators.values()
    }
}
