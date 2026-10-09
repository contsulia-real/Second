//! Bounded transition/body transport using the existing authenticated source pull.
use super::*;

const DOMAIN: &[u8] = b"SECOND_TRANSITION_HANDOFF_V1\0";

#[cfg(test)]
mod tests;
pub(crate) const MAX_SOURCE_SIZE: usize = crate::persistence::task_handoff::MAX_HANDOFF_SIZE
    + crate::MAX_VALIDATOR_TRANSITION_SOURCE_SIZE
    + 5
    + DOMAIN.len();

pub(crate) fn is_handoff_source(bytes: &[u8]) -> bool {
    bytes.starts_with(DOMAIN)
}

pub(crate) fn source_chunk(
    snapshot: &crate::PersistedNodeState,
    scope: &ConsensusScope,
    digest: [u8; 32],
    offset: u64,
) -> Option<(u64, Vec<u8>)> {
    let (handoff, source, collecting) = if let Some(transition) = snapshot
        .pending_governance
        .get(&digest)
        .and_then(crate::persistence::PendingGovernance::transition)
    {
        if &transition.scope() != scope {
            return None;
        }
        (
            transition.handoff.as_ref()?,
            ValidatorSetTransitionSource::from_transition(transition),
            matches!(
                snapshot.pending_governance.get(&digest),
                Some(crate::persistence::PendingGovernance::CollectingTransition(
                    _
                ))
            ),
        )
    } else {
        // Snapshot validation already ties this installed body to the original
        // committee's proof. Reuse it after activation without rehydrating from
        // today's business state or retaining another copy of the handoff.
        let handoff = snapshot.state.protocol.task_handoff.as_ref()?;
        let certifier = handoff.certifier_set.as_ref()?;
        let source = snapshot
            .validator_transition_proofs
            .get(&certifier.version())?
            .source();
        if source.next_validator_set() != &snapshot.validator_set
            || scope
                != &(ConsensusScope::CurrencyAllocation {
                    validator_set_version: certifier.version(),
                    start: source.currency_frontier(),
                })
            || crate::validator_transition::transition_digest(
                source.protocol_version(),
                certifier.version(),
                source.next_validator_set(),
                source.currency_frontier(),
                source.task_handoff_digest(),
            ) != digest
        {
            return None;
        }
        (handoff, source.clone(), false)
    };
    let body = handoff.encode_shared().ok()?;
    let source = source.encode_bytes().ok()?;
    let mut prefix = DOMAIN.to_vec();
    prefix.push(u8::from(collecting));
    prefix.extend_from_slice(&(source.len() as u32).to_be_bytes());
    prefix.extend_from_slice(&source);
    let total = prefix.len().checked_add(body.len())?;
    if total > MAX_SOURCE_SIZE {
        return None;
    }
    let offset = usize::try_from(offset).ok()?;
    if offset >= total {
        return None;
    }
    let end = offset
        .saturating_add(crate::network::MAX_PREPARED_TASK_SOURCE_CHUNK_SIZE)
        .min(total);
    let mut bytes = Vec::with_capacity(end - offset);
    if offset < prefix.len() {
        bytes.extend_from_slice(&prefix[offset..end.min(prefix.len())]);
    }
    if end > prefix.len() {
        bytes.extend_from_slice(&body[offset.saturating_sub(prefix.len())..end - prefix.len()]);
    }
    Some((total as u64, bytes))
}

impl NodeRuntime {
    pub(crate) fn install_transition_handoff_source(
        &self,
        version: u64,
        scope: &ConsensusScope,
        digest: [u8; 32],
        bytes: &[u8],
    ) -> Result<(), BftConsensusRuntimeError> {
        if bytes.len() > MAX_SOURCE_SIZE || !is_handoff_source(bytes) {
            return Err(BftConsensusRuntimeError::InvalidGovernanceSource);
        }
        let collecting = match bytes.get(DOMAIN.len()) {
            Some(0) => false,
            Some(1) => true,
            _ => return Err(BftConsensusRuntimeError::InvalidGovernanceSource),
        };
        let start = DOMAIN.len() + 1;
        let len = bytes
            .get(start..start + 4)
            .ok_or(BftConsensusRuntimeError::InvalidGovernanceSource)?;
        let len = u32::from_be_bytes(len.try_into().unwrap()) as usize;
        if len > crate::MAX_VALIDATOR_TRANSITION_SOURCE_SIZE {
            return Err(BftConsensusRuntimeError::InvalidGovernanceSource);
        }
        let start = start + 4;
        let source = ValidatorSetTransitionSource::decode_bytes(
            bytes
                .get(start..start + len)
                .ok_or(BftConsensusRuntimeError::InvalidGovernanceSource)?,
        )
        .map_err(BftConsensusRuntimeError::GovernanceSourceCodec)?;
        let context = self
            .governance_context()
            .ok_or(BftConsensusRuntimeError::InvalidGovernanceSource)?;
        let snapshot = context.current().map_err(node_error_to_consensus)?;
        if snapshot.validator_set.version() != version {
            return Err(BftConsensusRuntimeError::InvalidGovernanceSource);
        }
        let transition = source
            .verify(&snapshot.validator_set, &snapshot.validator_registry)
            .map_err(|_| BftConsensusRuntimeError::InvalidGovernanceSource)?;
        if &transition.scope() != scope || transition.digest() != digest {
            return Err(BftConsensusRuntimeError::InvalidGovernanceSource);
        }
        validate_bootstrap_quorum(&snapshot.validator_set, transition.next_validator_set())?;
        let handoff = crate::persistence::TaskHandoff::decode(&bytes[start + len..])
            .map_err(BftConsensusRuntimeError::Persistence)?;
        let transition = transition
            .with_handoff_arc(std::sync::Arc::new(handoff))
            .map_err(BftConsensusRuntimeError::Persistence)?;
        if transition.handoff_digest != source.task_handoff_digest() {
            return Err(BftConsensusRuntimeError::InvalidGovernanceSource);
        }
        if let Some(certificate) = context
            .runtime
            .consensus()
            .pending_finality_certificate(scope, digest)
        {
            context
                .install_transition_certificate(&transition, &certificate)
                .map_err(node_error_to_consensus)?;
            return Ok(());
        }
        if collecting {
            context
                .collect_transition(&transition)
                .map_err(node_error_to_consensus)?;
            context.runtime.remember_collected_transition(scope, digest);
            return Ok(());
        }
        context
            .begin_transition(&transition)
            .map_err(node_error_to_consensus)?;
        context.runtime.remember_collected_transition(scope, digest);
        Ok(())
    }
}
