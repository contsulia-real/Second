//! Replay pending source metadata to one newly authenticated committee peer.
use super::*;
use crate::ConsensusScope;

impl ValidatorBftRuntime {
    pub(super) fn replay_pending_sources(
        &self,
        recipient: ValidatorId,
        snapshot: &crate::PersistedNodeState,
    ) {
        for pending in
            crate::persistence::PendingGovernance::transition_sources(&snapshot.pending_governance)
        {
            let Some(transition) = pending.transition() else {
                continue;
            };
            if transition.current_validator_set_version() != snapshot.validator_set.version() {
                continue;
            }
            let Ok(bytes) =
                crate::ValidatorSetTransitionSource::from_transition(transition).encode_bytes()
            else {
                continue;
            };
            // Solicit this member's contribution without changing our admitted
            // body or vote locks. A body fetch retains its persisted phase.
            self.send_initial_evidence(
                recipient,
                BftNetworkMessage::ValidatorSetTransitionSource {
                    collecting: true,
                    validator_set_version: snapshot.validator_set.version(),
                    scope: transition.scope(),
                    bytes,
                },
            );
        }
        if snapshot.pending_governance.values().any(|pending| {
            matches!(pending, crate::persistence::PendingGovernance::CollectingTransition(value)
                if value.current_validator_set_version() == snapshot.validator_set.version())
        }) {
            // The collection source already carries every frozen body. Sending
            // separate business announcements here duplicates that exchange and
            // starts unsolicited resource acquisition before its root is sealed.
            // Explicit live business consensus retains its own announcement path.
            return;
        }
        for plan in snapshot.prepared_tasks.values() {
            if snapshot.state.task_succeeded(plan.task_id.clone()) == Some(true)
                || snapshot.state.task_cancelled(plan.task_id.clone())
            {
                continue;
            }
            let Some(origin) = crate::persistence::resolve_validator_set(
                &snapshot.validator_set,
                &snapshot.retained_validator_sets,
                plan.validator_set_version,
            ) else {
                continue;
            };
            if !origin.contains(recipient) || !origin.contains(self.validator_id()) {
                continue;
            }
            let Ok(digests) = plan.owned_candidate_digests() else {
                continue;
            };
            for digest in digests {
                self.send_initial_evidence(
                    recipient,
                    BftNetworkMessage::PreparedTaskAvailable {
                        validator_set_version: origin.version(),
                        scope: ConsensusScope::PreparedTask(plan.task_id.clone()),
                        expected_plan_digest: digest,
                        round: 0,
                    },
                );
            }
        }
    }
}
