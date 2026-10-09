use std::collections::BTreeMap;

#[cfg(test)]
mod collection_tests;

use super::codec::{Decoder, SnapshotContents, push_len};
use super::store::StateStore;
use crate::runtime_consensus_target::ValidatorConsensusTarget;
use crate::{
    PersistenceError, StateRecoveryCheckpoint, ValidatorRegistry, ValidatorSet,
    ValidatorSetTransition, ValidatorSetTransitionSource,
};

// Local admission records, excluded from the shared recovery payload.
#[derive(Clone)]
pub(crate) enum PendingGovernance {
    CollectingTransition(ValidatorSetTransition),
    Transition(ValidatorSetTransition),
    Recovery(StateRecoveryCheckpoint),
}

pub(super) const MAX_PENDING_GOVERNANCE: usize = 64;

pub(super) fn refresh_collecting(
    contents: &SnapshotContents<'_>,
    previous: &super::PersistedNodeState,
) -> Result<BTreeMap<[u8; 32], PendingGovernance>, PersistenceError> {
    // Source admission already captured and validated the new root. Reuse it
    // instead of encoding the same baseline and witnesses a second time.
    let supplied = contents
        .pending_governance
        .into_iter()
        .flat_map(|map| map.iter())
        .find_map(|(digest, pending)| match pending {
            PendingGovernance::CollectingTransition(value)
                if value.current_validator_set_version() == contents.validator_set.version()
                    && value.currency_frontier() == contents.state.next_currency_address()
                    && !previous.pending_governance.contains_key(digest) =>
            {
                value.handoff.clone()
            }
            _ => None,
        });
    let handoff = match supplied {
        Some(handoff) => handoff,
        None => std::sync::Arc::new(super::TaskHandoff::capture_parts(
            contents.state,
            contents.validator_set,
            contents.prepared_tasks,
        )?),
    };
    let mut pending = BTreeMap::new();
    for (digest, value) in contents
        .pending_governance
        .into_iter()
        .flat_map(|map| map.iter())
    {
        if let PendingGovernance::CollectingTransition(transition) = value
            && transition.current_validator_set_version() == contents.validator_set.version()
        {
            // No proposal or vote exists in collection. Keep the intent, but
            // derive its unsigned frontier/body from this same atomic write.
            let updated = ValidatorSetTransition::new(
                transition.protocol_version(),
                contents.validator_set,
                contents.validator_registry,
                transition.next_validator_set().clone(),
                transition.admissions().to_vec(),
                transition.consensus_key_rotations().to_vec(),
                contents.state.next_currency_address(),
            )
            .map_err(PersistenceError::ValidatorTransition)?
            .with_handoff_arc(handoff.clone())?;
            if pending
                .insert(
                    updated.digest(),
                    PendingGovernance::CollectingTransition(updated),
                )
                .is_some()
            {
                return Err(PersistenceError::InvalidSnapshot);
            }
        } else if pending.insert(*digest, value.clone()).is_some() {
            return Err(PersistenceError::InvalidSnapshot);
        }
    }
    Ok(pending)
}

impl PendingGovernance {
    pub(crate) fn transition_sources(pending: &BTreeMap<[u8; 32], Self>) -> Vec<&Self> {
        let mut preferred = BTreeMap::new();
        for value in pending.values() {
            let Some(transition) = value.transition() else {
                continue;
            };
            let intent = transition.clone().with_handoff_digest(None).digest();
            let duties = transition
                .handoff
                .as_ref()
                .map_or(0, |body| body.plans.len() + body.requests.len());
            let priority = (
                duties,
                matches!(value, Self::Transition(_)),
                transition.digest(),
            );
            let entry = preferred.entry(intent).or_insert((priority, value));
            if priority > entry.0 {
                *entry = (priority, value);
            }
        }
        preferred.into_values().map(|(_, value)| value).collect()
    }

    pub(crate) fn target(&self) -> Option<ValidatorConsensusTarget> {
        match self {
            Self::CollectingTransition(_) => None,
            Self::Transition(value) => Some(ValidatorConsensusTarget::ValidatorSetTransition(
                value.clone(),
            )),
            Self::Recovery(value) => Some(ValidatorConsensusTarget::StateRecoveryCheckpoint(
                value.clone(),
            )),
        }
    }

    pub(crate) fn transition(&self) -> Option<&ValidatorSetTransition> {
        match self {
            Self::CollectingTransition(value) | Self::Transition(value) => Some(value),
            Self::Recovery(_) => None,
        }
    }

    pub(super) fn retained(&self, contents: &SnapshotContents<'_>) -> bool {
        match self {
            Self::CollectingTransition(value) | Self::Transition(value) => {
                value.current_validator_set_version() == contents.validator_set.version()
                    && value.currency_frontier() == contents.state.next_currency_address()
            }
            Self::Recovery(value) => {
                value.validator_set_version() == contents.validator_set.version()
                    && !contents
                        .recovery_checkpoint_floors
                        .get(&value.validator_set_version())
                        .is_some_and(|floor| floor.certified && floor.serial >= value.serial())
            }
        }
    }
}

impl StateStore {
    pub(crate) fn promote_transition_collection(
        &self,
        transition: &ValidatorSetTransition,
    ) -> Result<bool, PersistenceError> {
        let _guard = self.lock()?;
        let mut latest = self
            .load_unlocked()?
            .ok_or(PersistenceError::MissingSnapshot)?;
        let target = ValidatorConsensusTarget::ValidatorSetTransition(transition.clone());
        match latest.pending_governance.get(&transition.digest()) {
            Some(PendingGovernance::Transition(value)) if value == transition => {
                super::bft_store::governance_subject(&latest, &target)?;
                return Ok(false);
            }
            Some(PendingGovernance::CollectingTransition(value)) if value == transition => {}
            _ => return Err(PersistenceError::StalePreparedTasks),
        }
        // Validate the current baseline and every collected obligation before
        // making this exact immutable root eligible for membership voting.
        super::task_handoff::hydrate(&latest, transition)?;
        latest.pending_governance.insert(
            transition.digest(),
            PendingGovernance::Transition(transition.clone()),
        );
        super::bft_store::governance_subject(&latest, &target)?;
        self.write_local_metadata_unlocked(&latest)?;
        Ok(true)
    }

    pub(crate) fn admit_governance(
        &self,
        target: &ValidatorConsensusTarget,
    ) -> Result<(), PersistenceError> {
        let pending = match target {
            ValidatorConsensusTarget::ValidatorSetTransition(value) => {
                PendingGovernance::Transition(value.clone())
            }
            ValidatorConsensusTarget::StateRecoveryCheckpoint(value) => {
                PendingGovernance::Recovery(value.clone())
            }
            _ => return Ok(()),
        };
        let _guard = self.lock()?;
        let mut latest = self
            .load_unlocked()?
            .ok_or(PersistenceError::MissingSnapshot)?;
        let subject = super::bft_store::governance_subject(&latest, target)?;
        if let Some(existing) = latest.pending_governance.get(&subject.digest()) {
            let upgrade = matches!((existing, &pending),
                (PendingGovernance::Transition(old), PendingGovernance::Transition(new))
                    if old.handoff.is_none() && new.handoff.is_some());
            if !upgrade {
                return Ok(());
            }
        } else if latest.pending_governance.len() >= MAX_PENDING_GOVERNANCE {
            return Err(PersistenceError::SnapshotTooLarge);
        }
        latest.pending_governance.insert(subject.digest(), pending);
        self.write_local_metadata_unlocked(&latest)?;
        Ok(())
    }
}

pub(super) fn encode_pending(
    out: &mut Vec<u8>,
    pending: Option<&BTreeMap<[u8; 32], PendingGovernance>>,
) -> Result<(), PersistenceError> {
    let count = pending.map_or(0, BTreeMap::len);
    if count > MAX_PENDING_GOVERNANCE {
        return Err(PersistenceError::InvalidSnapshot);
    }
    push_len(out, count)?;
    for value in pending.into_iter().flat_map(|map| map.values()) {
        match value {
            PendingGovernance::CollectingTransition(transition)
            | PendingGovernance::Transition(transition) => {
                out.push(
                    if matches!(value, PendingGovernance::CollectingTransition(_)) {
                        3
                    } else {
                        1
                    },
                );
                let bytes = ValidatorSetTransitionSource::from_transition(transition)
                    .encode_bytes()
                    .map_err(|_| PersistenceError::InvalidSnapshot)?;
                push_len(out, bytes.len())?;
                out.extend_from_slice(&bytes);
                match &transition.handoff {
                    None => out.push(0),
                    Some(handoff) => {
                        out.push(1);
                        let bytes = handoff.encode()?;
                        push_len(out, bytes.len())?;
                        out.extend_from_slice(&bytes);
                    }
                }
            }
            PendingGovernance::Recovery(checkpoint) => {
                out.push(2);
                out.extend_from_slice(&checkpoint.encode_bytes());
            }
        }
    }
    Ok(())
}

pub(super) fn decode_pending(
    decoder: &mut Decoder<'_>,
    validators: &ValidatorSet,
    registry: &ValidatorRegistry,
    currency_frontier: u64,
) -> Result<BTreeMap<[u8; 32], PendingGovernance>, PersistenceError> {
    let count = decoder.read_u64()?;
    if count > MAX_PENDING_GOVERNANCE as u64 {
        return Err(PersistenceError::InvalidSnapshot);
    }
    let mut pending = BTreeMap::new();
    for _ in 0..count {
        let (digest, value) = match decoder.read_u8()? {
            tag @ (1 | 3) => {
                let len = usize::try_from(decoder.read_u64()?)
                    .map_err(|_| PersistenceError::InvalidSnapshot)?;
                let source = ValidatorSetTransitionSource::decode_bytes(decoder.read_exact(len)?)
                    .map_err(|_| PersistenceError::InvalidSnapshot)?;
                let mut transition = source
                    .verify(validators, registry)
                    .map_err(|_| PersistenceError::InvalidSnapshot)?;
                match decoder.read_u8()? {
                    0 => {}
                    1 => {
                        let size = decoder.read_len()?;
                        let handoff = super::TaskHandoff::decode(decoder.read_exact(size)?)?;
                        let digest = transition.handoff_digest;
                        transition = transition.with_handoff(handoff)?;
                        if transition.handoff_digest != digest {
                            return Err(PersistenceError::InvalidSnapshot);
                        }
                    }
                    _ => return Err(PersistenceError::InvalidSnapshot),
                }
                if transition.currency_frontier() != currency_frontier {
                    return Err(PersistenceError::InvalidSnapshot);
                }
                if tag == 3 && transition.handoff.is_none() {
                    return Err(PersistenceError::InvalidSnapshot);
                }
                let digest = transition.digest();
                let value = if tag == 3 {
                    PendingGovernance::CollectingTransition(transition)
                } else {
                    PendingGovernance::Transition(transition)
                };
                (digest, value)
            }
            2 => {
                let value = StateRecoveryCheckpoint::decode_bytes(decoder.read_exact(52)?)
                    .ok_or(PersistenceError::InvalidSnapshot)?;
                if value.validator_set_version() != validators.version()
                    || value.protocol_version() != crate::CURRENT_PROTOCOL_VERSION
                    || value.serial() == 0
                {
                    return Err(PersistenceError::InvalidSnapshot);
                }
                (value.digest(), PendingGovernance::Recovery(value))
            }
            _ => return Err(PersistenceError::InvalidSnapshot),
        };
        if pending.insert(digest, value).is_some() {
            return Err(PersistenceError::InvalidSnapshot);
        }
    }
    Ok(pending)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CURRENT_PROTOCOL_VERSION, SecondState, ValidatorCredential, ValidatorId};

    #[test]
    fn historical_admission_is_retained_but_unknown_stale_sources_are_rejected() {
        let key = |seed| {
            ed25519_dalek::SigningKey::from_bytes(&[seed; 32])
                .verifying_key()
                .to_bytes()
        };
        let credential =
            ValidatorCredential::new(ValidatorId::new(1), key(71), key(72), key(73)).unwrap();
        let validators = ValidatorSet::new(1, [credential.clone()]).unwrap();
        let base =
            std::env::temp_dir().join(format!("second-governance-boundary-{}", std::process::id()));
        let store = StateStore::new(&base);
        let state = SecondState::genesis([], 1);
        store.initialize(&state, &validators).unwrap();
        let original = store.load().unwrap().unwrap();
        let checkpoint = store.next_state_recovery_checkpoint().unwrap();
        let transition = ValidatorSetTransition::new(
            CURRENT_PROTOCOL_VERSION,
            &validators,
            &original.validator_registry,
            ValidatorSet::new(2, [credential]).unwrap(),
            vec![],
            vec![],
            state.next_currency_address(),
        )
        .unwrap();
        store
            .admit_governance(&ValidatorConsensusTarget::StateRecoveryCheckpoint(
                checkpoint.clone(),
            ))
            .unwrap();
        store
            .admit_governance(&ValidatorConsensusTarget::ValidatorSetTransition(
                transition,
            ))
            .unwrap();
        assert!(
            checkpoint
                .matches_persisted(&store.load().unwrap().unwrap())
                .unwrap()
        );
        let advanced = state.clone().with_reserve(1).unwrap();
        store
            .save_with_prepared(
                &state,
                &advanced,
                &validators,
                &BTreeMap::new(),
                &BTreeMap::new(),
            )
            .unwrap();
        for snapshot in [
            store.load().unwrap().unwrap(),
            StateStore::new(&base).load().unwrap().unwrap(),
        ] {
            assert_eq!(snapshot.pending_governance.len(), 1);
            assert!(
                matches!(snapshot.pending_governance.values().next(), Some(PendingGovernance::Recovery(value)) if value == &checkpoint)
            );
        }
        assert!(
            store
                .state_recovery_bft_proposal_subject(&checkpoint)
                .is_ok()
        );
        let replacement = store.next_state_recovery_checkpoint().unwrap();
        assert_eq!(replacement.serial(), checkpoint.serial());
        assert_ne!(replacement.digest(), checkpoint.digest());
        store
            .admit_governance(&ValidatorConsensusTarget::StateRecoveryCheckpoint(
                replacement,
            ))
            .unwrap();
        let unknown =
            StateRecoveryCheckpoint::from_untrusted_parts(CURRENT_PROTOCOL_VERSION, 1, 1, [93; 32]);
        let generation = store.load().unwrap().unwrap().generation;
        assert_eq!(
            store.admit_governance(&ValidatorConsensusTarget::StateRecoveryCheckpoint(unknown)),
            Err(PersistenceError::RecoveryCheckpointDoesNotMatchState)
        );
        assert_eq!(store.load().unwrap().unwrap().generation, generation);
        store.remove_files().unwrap();
    }
}
