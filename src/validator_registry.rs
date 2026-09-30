use std::collections::{BTreeMap, BTreeSet};

use crate::{ValidatorCredential, ValidatorId, ValidatorRegistryError, ValidatorSet};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ValidatorStatus {
    Active,
    Retired,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ValidatorRegistryRecord {
    pub(crate) credential: ValidatorCredential,
    pub(crate) status: ValidatorStatus,
    pub(crate) consensus_key_history: BTreeSet<[u8; 32]>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidatorRegistry {
    active_validator_set_version: u64,
    records: BTreeMap<ValidatorId, ValidatorRegistryRecord>,
    used_keys: BTreeSet<[u8; 32]>,
}

impl ValidatorRegistry {
    pub fn from_validator_set(
        validator_set: &ValidatorSet,
    ) -> Result<Self, ValidatorRegistryError> {
        let records = validator_set
            .credentials()
            .cloned()
            .map(|credential| {
                let consensus_key_history = BTreeSet::from([credential.consensus_public_key()]);
                ValidatorRegistryRecord {
                    credential,
                    status: ValidatorStatus::Active,
                    consensus_key_history,
                }
            })
            .collect::<Vec<_>>();

        Self::from_records(validator_set.version(), records)
    }

    pub const fn active_validator_set_version(&self) -> u64 {
        self.active_validator_set_version
    }

    pub fn contains(&self, validator_id: ValidatorId) -> bool {
        self.records.contains_key(&validator_id)
    }

    pub fn status(&self, validator_id: ValidatorId) -> Option<ValidatorStatus> {
        self.records.get(&validator_id).map(|record| record.status)
    }

    pub fn credential(&self, validator_id: ValidatorId) -> Option<&ValidatorCredential> {
        self.records
            .get(&validator_id)
            .map(|record| &record.credential)
    }

    pub fn validate_current_set(
        &self,
        current_validator_set: &ValidatorSet,
    ) -> Result<(), ValidatorRegistryError> {
        if current_validator_set.version() != self.active_validator_set_version {
            return Err(ValidatorRegistryError::CurrentSetVersionMismatch {
                expected: self.active_validator_set_version,
                actual: current_validator_set.version(),
            });
        }

        let active_count = self
            .records
            .values()
            .filter(|record| record.status == ValidatorStatus::Active)
            .count();
        if active_count != current_validator_set.len() {
            return Err(ValidatorRegistryError::CurrentSetMembershipMismatch);
        }

        for current in current_validator_set.credentials() {
            let Some(record) = self.records.get(&current.id()) else {
                return Err(ValidatorRegistryError::CurrentSetMembershipMismatch);
            };

            if record.status != ValidatorStatus::Active || record.credential != *current {
                return Err(ValidatorRegistryError::CurrentSetCredentialMismatch(
                    current.id(),
                ));
            }
        }

        Ok(())
    }

    pub fn validate_transition(
        &self,
        current_validator_set: &ValidatorSet,
        next_validator_set: &ValidatorSet,
    ) -> Result<(), ValidatorRegistryError> {
        self.validate_current_set(current_validator_set)?;

        for next in next_validator_set.credentials() {
            match self.records.get(&next.id()) {
                Some(record) if record.status == ValidatorStatus::Retired => {
                    return Err(ValidatorRegistryError::ValidatorIdAlreadyUsed(next.id()));
                }
                Some(record) => {
                    if next.identity_public_key() != record.credential.identity_public_key() {
                        return Err(ValidatorRegistryError::IdentityKeyChanged(next.id()));
                    }
                    if next.recovery_public_key() != record.credential.recovery_public_key() {
                        return Err(ValidatorRegistryError::RecoveryKeyChanged(next.id()));
                    }

                    let current_consensus = record.credential.consensus_public_key();
                    let next_consensus = next.consensus_public_key();
                    if next_consensus != current_consensus
                        && self.used_keys.contains(&next_consensus)
                    {
                        return Err(ValidatorRegistryError::ValidatorKeyAlreadyUsed(next.id()));
                    }
                }
                None => {
                    for key in [
                        next.identity_public_key(),
                        next.consensus_public_key(),
                        next.recovery_public_key(),
                    ] {
                        if self.used_keys.contains(&key) {
                            return Err(ValidatorRegistryError::ValidatorKeyAlreadyUsed(next.id()));
                        }
                    }
                }
            }
        }

        Ok(())
    }

    pub(crate) fn apply_next_set(
        &mut self,
        next_validator_set: &ValidatorSet,
    ) -> Result<(), ValidatorRegistryError> {
        let expected_version = self
            .active_validator_set_version
            .checked_add(1)
            .ok_or(ValidatorRegistryError::ValidatorSetVersionOverflow)?;
        if next_validator_set.version() != expected_version {
            return Err(ValidatorRegistryError::WrongNextValidatorSetVersion {
                expected: expected_version,
                actual: next_validator_set.version(),
            });
        }

        let current = ValidatorSet::new(
            self.active_validator_set_version,
            self.records
                .values()
                .filter(|record| record.status == ValidatorStatus::Active)
                .map(|record| record.credential.clone()),
        )
        .map_err(|_| ValidatorRegistryError::CurrentSetMembershipMismatch)?;

        self.validate_transition(&current, next_validator_set)?;

        for record in self.records.values_mut() {
            if record.status == ValidatorStatus::Active
                && !next_validator_set.contains(record.credential.id())
            {
                record.status = ValidatorStatus::Retired;
            }
        }

        for next in next_validator_set.credentials() {
            if let Some(record) = self.records.get_mut(&next.id()) {
                let next_consensus = next.consensus_public_key();
                if next_consensus != record.credential.consensus_public_key() {
                    self.used_keys.insert(next_consensus);
                    record.consensus_key_history.insert(next_consensus);
                    record.credential = next.clone();
                }
                record.status = ValidatorStatus::Active;
            } else {
                for key in [
                    next.identity_public_key(),
                    next.consensus_public_key(),
                    next.recovery_public_key(),
                ] {
                    self.used_keys.insert(key);
                }

                self.records.insert(
                    next.id(),
                    ValidatorRegistryRecord {
                        credential: next.clone(),
                        status: ValidatorStatus::Active,
                        consensus_key_history: BTreeSet::from([next.consensus_public_key()]),
                    },
                );
            }
        }

        self.active_validator_set_version = next_validator_set.version();
        Ok(())
    }

    pub(crate) fn records(&self) -> impl Iterator<Item = (&ValidatorId, &ValidatorRegistryRecord)> {
        self.records.iter()
    }

    pub(crate) fn from_records(
        active_validator_set_version: u64,
        records: Vec<ValidatorRegistryRecord>,
    ) -> Result<Self, ValidatorRegistryError> {
        let mut by_id = BTreeMap::new();
        let mut used_keys = BTreeSet::new();

        for record in records {
            let validator_id = record.credential.id();
            if by_id.contains_key(&validator_id) {
                return Err(ValidatorRegistryError::ValidatorIdAlreadyUsed(validator_id));
            }

            if record.consensus_key_history.is_empty()
                || !record
                    .consensus_key_history
                    .contains(&record.credential.consensus_public_key())
            {
                return Err(ValidatorRegistryError::InvalidConsensusKeyHistory(
                    validator_id,
                ));
            }

            for key in [
                record.credential.identity_public_key(),
                record.credential.recovery_public_key(),
            ] {
                if !used_keys.insert(key) {
                    return Err(ValidatorRegistryError::ValidatorKeyAlreadyUsed(
                        validator_id,
                    ));
                }
            }

            for key in &record.consensus_key_history {
                if !used_keys.insert(*key) {
                    return Err(ValidatorRegistryError::ValidatorKeyAlreadyUsed(
                        validator_id,
                    ));
                }
            }

            by_id.insert(validator_id, record);
        }

        if !by_id
            .values()
            .any(|record| record.status == ValidatorStatus::Active)
        {
            return Err(ValidatorRegistryError::CurrentSetMembershipMismatch);
        }

        Ok(Self {
            active_validator_set_version,
            records: by_id,
            used_keys,
        })
    }
}
