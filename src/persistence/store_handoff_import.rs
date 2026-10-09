//! Install a certified transition baseline without importing local signing history.
use super::{PersistedNodeState, StateStore, TaskHandoff};
use crate::{AuthorizerSet, PersistenceError, StateRecoveryPayload, ValidatorSetTransitionProof};
use std::collections::BTreeMap;
use std::sync::Arc;

#[cfg(test)]
mod tests;

impl StateStore {
    /// `trusted` is a locally trusted committee/registry anchor, not peer input.
    /// Installing shared state always leaves voting locked, as recovery does.
    pub fn install_validator_handoff_baseline(
        &self,
        proof: &ValidatorSetTransitionProof,
        handoff_bytes: &[u8],
        trusted: &PersistedNodeState,
        authorizers: &AuthorizerSet,
    ) -> Result<u64, PersistenceError> {
        let _guard = self.lock()?;
        if self.load_unlocked()?.is_some() {
            return Err(PersistenceError::AlreadyInitialized);
        }
        let transition = proof
            .source()
            .verify(&trusted.validator_set, &trusted.validator_registry)
            .map_err(|_| PersistenceError::InvalidSnapshot)?;
        let handoff = Arc::new(TaskHandoff::decode(handoff_bytes)?);
        if handoff.certifier_set.as_ref() != Some(&trusted.validator_set)
            || proof.source().task_handoff_digest()
                != if handoff.requires_commitment() {
                    Some(handoff.digest()?)
                } else {
                    None
                }
        {
            return Err(PersistenceError::InvalidSnapshot);
        }
        let mut state = if handoff.business_baseline_bytes().is_empty() {
            crate::SecondState::genesis([], 1)
        } else {
            super::codec::decode_handoff_business_baseline(handoff.business_baseline_bytes())?
        };
        if state.next_currency_address() != transition.currency_frontier() {
            return Err(PersistenceError::StaleState);
        }
        let mut sources = BTreeMap::new();
        handoff.admit_requests(&mut state, authorizers)?;
        for ((task_id, _), plan) in &handoff.plans {
            if let Some(previous) = sources.get(task_id) {
                if *previous != &plan.source_task {
                    return Err(PersistenceError::InvalidSnapshot);
                }
                continue;
            }
            let verified = plan
                .source_task
                .clone()
                .verify(authorizers)
                .map_err(|_| PersistenceError::InvalidSnapshot)?;
            state
                .authorize_task(&verified)
                .map_err(|_| PersistenceError::InvalidSnapshot)?;
            if verified.task_id() != *task_id || verified.request_digest() != plan.request_digest {
                return Err(PersistenceError::InvalidSnapshot);
            }
            state
                .bind_task(&verified)
                .map_err(|_| PersistenceError::InvalidSnapshot)?;
            sources.insert(task_id.clone(), &plan.source_task);
        }
        if handoff.requires_commitment() {
            state.protocol.task_handoff = Some(handoff);
        }
        let next = transition.next_validator_set();
        let proofs = BTreeMap::from([(trusted.validator_set.version(), proof.clone())]);
        // This verifies the original quorum once and caches its exact root/proof
        // fingerprint. The normal snapshot writer reuses that validation.
        super::task_handoff::validate_installed(&state, next, &proofs, None)?;
        // Empty genesis handoffs have no installed body to validate above.
        if state.protocol.task_handoff.is_none() {
            crate::CertifiedValidatorSetTransition::new(
                transition.clone(),
                proof.votes().to_vec(),
                &trusted.validator_set,
            )
            .map_err(PersistenceError::ValidatorTransition)?;
        }
        let mut registry = trusted.validator_registry.clone();
        registry
            .apply_next_set(next)
            .map_err(|_| PersistenceError::ValidatorRegistryMismatch)?;
        let retained = super::store_prepared::retained_sets_for_prepared(
            Some(trusted),
            next,
            &state,
            &BTreeMap::new(),
            &BTreeMap::new(),
        )?;
        let mut payload =
            StateRecoveryPayload::from_shared_parts(&state, next, &registry, &retained)?;
        payload.validator_transition_proofs = proofs;
        self.write_initial_shared_state_unlocked(&payload, None)
    }
}
