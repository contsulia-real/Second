//! Immutable membership handoff candidates. These are fences, not Commit rights.
mod codec;
mod requests;
mod retirement;
use crate::prepared_plan::{PreparedTask, PreparedTaskPhase};
use crate::{PersistedNodeState, PersistenceError, TaskId};
pub(super) use retirement::hydrate_certified;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

const DOMAIN: &[u8] = b"SECOND_TASK_HANDOFF_V1\0";
pub(crate) const MAX_HANDOFF_SIZE: usize = 512 * 1024 * 1024;

#[derive(Clone, Debug, Default)]
pub(crate) struct TaskHandoff {
    pub(crate) plans: BTreeMap<(TaskId, [u8; 32]), PreparedTask>,
    pub(crate) requests: BTreeMap<TaskId, crate::LegalTask>,
    pub(crate) certifier_set: Option<crate::ValidatorSet>,
    business_digest: Option<[u8; 32]>,
    business_baseline: Option<std::sync::Arc<Vec<u8>>>,
    index: std::sync::OnceLock<crate::prepared::fences::ResourceFenceIndex>,
    verified_proof: std::sync::OnceLock<[u8; 32]>,
    encoded: std::sync::OnceLock<std::sync::Arc<Vec<u8>>>,
}

impl PartialEq for TaskHandoff {
    fn eq(&self, other: &Self) -> bool {
        self.plans == other.plans
            && self.requests == other.requests
            && self.certifier_set == other.certifier_set
            && self.business_digest == other.business_digest
            && self.business_baseline == other.business_baseline
    }
}
impl Eq for TaskHandoff {}

impl super::StateStore {
    pub fn prepare_validator_set_transition(
        &self,
        transition: crate::ValidatorSetTransition,
    ) -> Result<crate::ValidatorSetTransition, PersistenceError> {
        let snapshot = self
            .load_shared()?
            .ok_or(PersistenceError::MissingSnapshot)?;
        hydrate(&snapshot, &transition)
    }
}

pub(super) fn hydrate(
    snapshot: &PersistedNodeState,
    transition: &crate::ValidatorSetTransition,
) -> Result<crate::ValidatorSetTransition, PersistenceError> {
    let hydrated = resolve(snapshot, transition)?;
    hydrated.handoff.as_ref().unwrap().covers(snapshot)?;
    Ok(hydrated)
}

fn resolve(
    snapshot: &PersistedNodeState,
    transition: &crate::ValidatorSetTransition,
) -> Result<crate::ValidatorSetTransition, PersistenceError> {
    let handoff = if let Some(handoff) = &transition.handoff {
        handoff.clone()
    } else if let Some(saved) = snapshot
        .pending_governance
        .get(&transition.digest())
        .and_then(super::PendingGovernance::transition)
        && let Some(handoff) = &saved.handoff
    {
        handoff.clone()
    } else {
        std::sync::Arc::new(TaskHandoff::capture(snapshot)?)
    };
    if handoff.certifier_set.as_ref() != Some(&snapshot.validator_set) {
        return Err(PersistenceError::ValidatorRegistryMismatch);
    }
    let requested = transition.handoff_digest;
    let hydrated = transition.clone().with_handoff_arc(handoff)?;
    if requested.is_some() && hydrated.handoff_digest != requested {
        return Err(PersistenceError::StalePreparedTasks);
    }
    Ok(hydrated)
}

impl TaskHandoff {
    pub(crate) fn empty_for(certifier: &crate::ValidatorSet) -> Self {
        Self {
            certifier_set: Some(certifier.clone()),
            ..Default::default()
        }
    }
    pub(crate) fn task_context(&self, task_id: &TaskId) -> Option<&PreparedTask> {
        self.plans
            .range((task_id.clone(), [0; 32])..=(task_id.clone(), [u8::MAX; 32]))
            .next()
            .map(|(_, plan)| plan)
    }

    pub(crate) fn validate_task_origin(
        &self,
        state: &crate::SecondState,
        task_id: &TaskId,
        actual: u64,
    ) -> Result<(), crate::PreparationError> {
        if state
            .protocol
            .task_bindings
            .get(task_id)
            .is_some_and(|binding| binding.outcome.is_terminal())
        {
            return Ok(());
        }
        if let Some(plan) = self.task_context(task_id)
            && plan.validator_set_version != actual
        {
            return Err(crate::PreparationError::TaskOriginMismatch {
                expected: plan.validator_set_version,
                actual,
            });
        }
        Ok(())
    }
    pub(crate) fn capture(snapshot: &PersistedNodeState) -> Result<Self, PersistenceError> {
        Self::capture_parts(
            &snapshot.state,
            &snapshot.validator_set,
            &snapshot.prepared_tasks,
        )
    }

    pub(super) fn capture_parts(
        state: &crate::SecondState,
        validators: &crate::ValidatorSet,
        prepared: &BTreeMap<TaskId, PreparedTask>,
    ) -> Result<Self, PersistenceError> {
        let baseline = business_baseline(state)?;
        let mut value = Self {
            certifier_set: Some(validators.clone()),
            business_digest: baseline.as_deref().map(hash_business_baseline),
            business_baseline: baseline.map(std::sync::Arc::new),
            ..Self::default()
        };
        if let Some(previous) = &state.protocol.task_handoff {
            for plan in previous.plans.values() {
                if !state
                    .protocol
                    .task_bindings
                    .get(&plan.task_id)
                    .is_some_and(|binding| binding.outcome.is_terminal())
                {
                    value.insert(plan.clone())?;
                }
            }
        }
        for plan in prepared.values() {
            value.insert(plan.clone())?;
        }
        if let Some(previous) = &state.protocol.task_handoff {
            for task in previous.requests.values() {
                if !state
                    .protocol
                    .task_bindings
                    .get(&task.payload().task_id())
                    .is_some_and(|binding| binding.outcome.is_terminal())
                {
                    value.insert_request(task.clone())?;
                }
            }
        }
        for binding in state.protocol.task_bindings.values() {
            if !binding.outcome.is_terminal()
                && let Some(task) = &binding.allocation_task
            {
                value.insert_request(task.clone())?;
            }
        }
        Ok(value)
    }

    pub(crate) fn insert(&mut self, mut plan: PreparedTask) -> Result<(), PersistenceError> {
        // Local candidate selection and arrival order are not handoff truth.
        // Each frozen body has its own canonical key; include every obligation.
        let variants = std::mem::take(&mut plan.variants);
        for variant in variants {
            let mut candidate = plan.clone();
            candidate.operations = variant.operations;
            self.insert(candidate)?;
        }
        let digest = plan
            .plan_digest()
            .map_err(|_| PersistenceError::InvalidSnapshot)?;
        if self.task_context(&plan.task_id).is_some_and(|other| {
            other.request_digest != plan.request_digest
                || other.validator_set_version != plan.validator_set_version
        }) {
            return Err(PersistenceError::InvalidSnapshot);
        }
        // Voting phase and quorum subset are local evidence, not part of the
        // canonical resource obligation agreed by the old committee.
        plan.phase = PreparedTaskPhase::Prepared;
        plan.commit_authorized = true;
        plan.conflict_abort = false;
        plan.finality_votes = None;
        let key = (plan.task_id.clone(), digest);
        if let Some(request) = self.requests.get(&plan.task_id) {
            if request != &plan.source_task {
                return Err(PersistenceError::InvalidSnapshot);
            }
            self.requests.remove(&plan.task_id);
        }
        if let Some(previous) = self.plans.get(&key) {
            if previous != &plan {
                return Err(PersistenceError::InvalidSnapshot);
            }
            return Ok(());
        }
        self.plans.insert(key, plan);
        self.index.take();
        self.encoded.take();
        self.verified_proof.take();
        Ok(())
    }

    pub(crate) fn blockers(
        &self,
        state: &crate::SecondState,
        candidate: &PreparedTask,
    ) -> Result<Vec<TaskId>, crate::PreparationError> {
        if self.index.get().is_none() {
            let index =
                crate::prepared::fences::ResourceFenceIndex::for_handoff(self.plans.values())?;
            let _ = self.index.set(index);
        }
        let index = self.index.get().unwrap();
        let mut blockers = index.blockers(state, candidate)?;
        blockers.extend(index.inherited_execution_blockers(state, candidate));
        Ok(blockers.into_iter().collect())
    }

    pub(crate) fn covers(&self, snapshot: &PersistedNodeState) -> Result<(), PersistenceError> {
        self.validate_business_baseline(snapshot)?;
        let admitted = snapshot
            .pending_governance
            .values()
            .find_map(|pending| match pending {
                super::PendingGovernance::Transition(value) => value
                    .handoff
                    .as_ref()
                    .filter(|saved| saved.as_ref() == self),
                _ => None,
            });
        self.covers_plans(snapshot, admitted.map(|saved| saved.as_ref()))
    }

    pub(crate) fn validate_business_baseline(
        &self,
        snapshot: &PersistedNodeState,
    ) -> Result<(), PersistenceError> {
        if self.certifier_set.as_ref() != Some(&snapshot.validator_set) {
            return Err(PersistenceError::ValidatorRegistryMismatch);
        }
        // Admitted candidates survive completion of their already covered tasks.
        // New candidates must attest the current shared business baseline.
        let admitted = snapshot
            .pending_governance
            .values()
            .find_map(|pending| match pending {
                super::PendingGovernance::Transition(value) => value
                    .handoff
                    .as_ref()
                    .filter(|saved| saved.as_ref() == self),
                _ => None,
            });
        if admitted.is_none() && self.business_digest != business_digest(&snapshot.state)? {
            return Err(PersistenceError::StaleState);
        }
        Ok(())
    }

    fn covers_plans(
        &self,
        snapshot: &PersistedNodeState,
        admitted: Option<&Self>,
    ) -> Result<(), PersistenceError> {
        self.covers_requests(snapshot, admitted)?;
        // Only locally validated bodies (including authorized handoff imports)
        // or inherited certified obligations can satisfy a proposed handoff.
        // A peer-supplied resource list must not create Commit or fence rights.
        for (key, plan) in &self.plans {
            let local = snapshot
                .prepared_tasks
                .get(&plan.task_id)
                .map(|local| local.candidate(key.1))
                .transpose()
                .map_err(|_| PersistenceError::InvalidSnapshot)?
                .flatten();
            let inherited = snapshot
                .state
                .protocol
                .task_handoff
                .as_ref()
                .and_then(|previous| previous.plans.get(key));
            let remembered = admitted.and_then(|saved| saved.plans.get(key)).filter(|_| {
                snapshot
                    .state
                    .protocol
                    .task_bindings
                    .get(&plan.task_id)
                    .is_some_and(|binding| binding.outcome.is_terminal())
            });
            let expected = local
                .as_ref()
                .or(inherited)
                .or(remembered)
                .ok_or(PersistenceError::StalePreparedTasks)?;
            let mut expected = expected.clone();
            expected.phase = PreparedTaskPhase::Prepared;
            expected.commit_authorized = true;
            expected.conflict_abort = false;
            expected.finality_votes = None;
            expected.variants.clear();
            if plan != &expected {
                return Err(PersistenceError::InvalidSnapshot);
            }
            let binding = snapshot
                .state
                .protocol
                .task_bindings
                .get(&plan.task_id)
                .ok_or(PersistenceError::InvalidSnapshot)?;
            if binding.request_digest != plan.request_digest {
                return Err(PersistenceError::InvalidSnapshot);
            }
        }
        let protected = super::contention::handoff_evidence_digests(snapshot)?;
        self.covers_evidence(snapshot, &protected)?;
        for plan in snapshot.prepared_tasks.values() {
            let required = admitted
                .map(|_| {
                    let mut digests = plan.owned_candidate_digests()?;
                    if plan.phase == PreparedTaskPhase::Finalized {
                        digests.push(plan.plan_digest()?);
                    }
                    Ok::<_, crate::PreparationError>(digests)
                })
                .transpose()
                .map_err(|_| PersistenceError::InvalidSnapshot)?;
            let mut obligations = Self::default();
            obligations.insert(plan.clone())?;
            for (key, expected) in obligations.plans {
                if required
                    .as_ref()
                    .is_some_and(|digests| !digests.contains(&key.1))
                {
                    // A later foreign witness cannot invalidate an immutable
                    // admitted candidate. Owned and certified duties remain required.
                    continue;
                }
                if self.plans.get(&key) != Some(&expected) {
                    return Err(PersistenceError::StalePreparedTasks);
                }
            }
        }
        if let Some(previous) = &snapshot.state.protocol.task_handoff {
            for (key, plan) in &previous.plans {
                if !snapshot
                    .state
                    .protocol
                    .task_bindings
                    .get(&plan.task_id)
                    .is_some_and(|binding| binding.outcome.is_terminal())
                    && self.plans.get(key) != Some(plan)
                {
                    return Err(PersistenceError::StalePreparedTasks);
                }
            }
        }
        Ok(())
    }

    pub(crate) fn business_baseline_bytes(&self) -> &[u8] {
        self.business_baseline
            .as_deref()
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub(crate) fn requires_commitment(&self) -> bool {
        !self.plans.is_empty() || !self.requests.is_empty() || self.business_digest.is_some()
    }
}

fn business_digest(state: &crate::SecondState) -> Result<Option<[u8; 32]>, PersistenceError> {
    Ok(business_baseline(state)?
        .as_deref()
        .map(hash_business_baseline))
}

fn business_baseline(state: &crate::SecondState) -> Result<Option<Vec<u8>>, PersistenceError> {
    let mut baseline = state.clone();
    baseline.protocol.task_handoff = None;
    baseline.prerequisite = Default::default();
    // Pending local bindings/prerequisites are covered by frozen plans. Preserve
    // certified allocations and terminal bindings, even with no active plan.
    baseline
        .protocol
        .task_bindings
        .retain(|_, binding| binding.outcome.is_terminal() || binding.allocation.is_some());
    for binding in baseline.protocol.task_bindings.values_mut() {
        // This is a local preparation retry source. The certified allocation's
        // range and request binding remain identical before and after it clears.
        binding.allocation_task = None;
    }
    // Only the canonical empty genesis has no business baseline to attest.
    // Genesis accounts/reserves and other unbound state still require a digest.
    if baseline.same_persisted_state(&crate::SecondState::genesis([], 1)) {
        return Ok(None);
    }
    let mut encoded = Vec::new();
    super::codec::encode_second_state(&mut encoded, &baseline)?;
    if encoded.len() > MAX_HANDOFF_SIZE {
        return Err(PersistenceError::SnapshotTooLarge);
    }
    Ok(Some(encoded))
}

fn hash_business_baseline(encoded: &[u8]) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(b"SECOND_HANDOFF_BUSINESS_BASE_V1\0");
    hash.update(encoded);
    hash.finalize().into()
}

pub(crate) fn validate_installed(
    state: &crate::SecondState,
    validators: &crate::ValidatorSet,
    proofs: &BTreeMap<u64, crate::ValidatorSetTransitionProof>,
    recovery: Option<&crate::StateRecoveryCheckpointProof>,
) -> Result<(), PersistenceError> {
    let Some(handoff) = &state.protocol.task_handoff else {
        return Ok(());
    };
    let certifier = handoff
        .certifier_set
        .as_ref()
        .ok_or(PersistenceError::InvalidSnapshot)?;
    if !handoff.requires_commitment()
        || certifier.version().checked_add(1) != Some(validators.version())
    {
        return Err(PersistenceError::InvalidSnapshot);
    }
    for plan in handoff.plans.values() {
        let binding = state
            .protocol
            .task_bindings
            .get(&plan.task_id)
            .ok_or(PersistenceError::InvalidSnapshot)?;
        if binding.request_digest != plan.request_digest {
            return Err(PersistenceError::InvalidSnapshot);
        }
    }
    for (task_id, task) in &handoff.requests {
        if state.bound_request_digest(task_id.clone())
            != Some(
                task.request_digest()
                    .map_err(|_| PersistenceError::InvalidSnapshot)?,
            )
        {
            return Err(PersistenceError::InvalidSnapshot);
        }
    }
    if let Some(proof) = proofs.get(&certifier.version()) {
        let source = proof.source();
        let root = handoff.digest()?;
        if source.task_handoff_digest() != Some(root) || source.next_validator_set() != validators {
            return Err(PersistenceError::InvalidSnapshot);
        }
        // Binding-only handoffs remain after ordinary task completion. Avoid
        // rechecking the same old quorum on every local BFT metadata write.
        // Rehash both body and proof so cloned/mutated inputs cannot reuse trust.
        let mut fingerprint = Sha256::new();
        fingerprint.update(b"SECOND_VERIFIED_HANDOFF_PROOF_V1\0");
        fingerprint.update(root);
        fingerprint.update(
            proof
                .encode_bytes()
                .map_err(|_| PersistenceError::InvalidSnapshot)?,
        );
        let fingerprint: [u8; 32] = fingerprint.finalize().into();
        if handoff.verified_proof.get() == Some(&fingerprint) {
            return Ok(());
        }
        let digest = crate::validator_transition::transition_digest(
            source.protocol_version(),
            certifier.version(),
            validators,
            source.currency_frontier(),
            Some(root),
        );
        let statement =
            crate::FinalityStatement::new(source.protocol_version(), certifier.version(), digest);
        crate::FinalityCertificate::new(statement, proof.votes().to_vec(), certifier)
            .map_err(|_| PersistenceError::InvalidSnapshot)?;
        let _ = handoff.verified_proof.set(fingerprint);
    } else if recovery.is_none() {
        // Recovery may replace old transition evidence only when its current
        // exact committee certificate attests the complete shared payload.
        return Err(PersistenceError::InvalidSnapshot);
    }
    Ok(())
}
