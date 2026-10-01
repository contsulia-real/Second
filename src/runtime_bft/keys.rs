use std::collections::BTreeMap;

use ed25519_dalek::SigningKey;

use crate::{ValidatorId, ValidatorSet};

#[derive(Clone)]
pub struct ValidatorRuntimeKeys {
    validator_id: ValidatorId,
    identity_key: SigningKey,
    consensus_keys: BTreeMap<[u8; 32], SigningKey>,
}

impl ValidatorRuntimeKeys {
    pub fn new(
        validator_id: ValidatorId,
        identity_key: SigningKey,
        consensus_key: SigningKey,
    ) -> Self {
        let consensus_public_key = consensus_key.verifying_key().to_bytes();
        Self {
            validator_id,
            identity_key,
            consensus_keys: BTreeMap::from([(consensus_public_key, consensus_key)]),
        }
    }

    pub fn with_consensus_key(mut self, consensus_key: SigningKey) -> Self {
        self.consensus_keys
            .insert(consensus_key.verifying_key().to_bytes(), consensus_key);
        self
    }

    pub const fn validator_id(&self) -> ValidatorId {
        self.validator_id
    }

    pub(crate) fn identity_key(&self) -> &SigningKey {
        &self.identity_key
    }

    pub(crate) fn consensus_key_for(&self, validator_set: &ValidatorSet) -> Option<&SigningKey> {
        let credential = validator_set.validator(self.validator_id)?;
        self.consensus_keys.get(&credential.consensus_public_key())
    }
}
