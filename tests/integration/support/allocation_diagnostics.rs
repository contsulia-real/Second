//! Failure-only allocation evidence, without request bodies or vote signatures.
use second::{BftConsensusEvent, ConsensusScope, NodeRuntime, StateStore, TaskId, ValidatorId};
use std::{collections::BTreeMap, path::Path};

pub fn report(
    runtime: &NodeRuntime,
    store: &StateStore,
    base: &Path,
    validator: ValidatorId,
    tasks: impl IntoIterator<Item = TaskId>,
) {
    let snapshot = store.load().unwrap().unwrap();
    let frontier = snapshot.state.next_currency_address();
    let version = snapshot.validator_set.version();
    let voting = |scope: &ConsensusScope| {
        store
            .bft_local_state(validator, scope)
            .unwrap()
            .as_ref()
            .map(|state| {
                (
                    state.round(),
                    state.locked_digest().is_some(),
                    state.valid_prevote_qc().is_some(),
                )
            })
    };
    let allocation = voting(&ConsensusScope::CurrencyAllocation {
        validator_set_version: version,
        start: frontier,
    });
    let tasks = tasks
        .into_iter()
        .map(|id| {
            (
                snapshot.state.task_succeeded(id.clone()),
                snapshot.state.task_cancelled(id.clone()),
                voting(&ConsensusScope::PreparedTask(id)),
            )
        })
        .collect::<Vec<_>>();
    let mut errors = BTreeMap::new();
    for event in runtime.drain_bft_consensus_events().unwrap() {
        let error = match event {
            BftConsensusEvent::Rejected { error, .. } => Some(format!("{error:?}")),
            BftConsensusEvent::ConnectionFailed { error, .. } => {
                Some(format!("connection: {error:?}"))
            }
            BftConsensusEvent::SendFailed { failures, .. } => {
                for failure in failures {
                    *errors
                        .entry(format!("send: {:?}", failure.error))
                        .or_insert(0_usize) += 1;
                }
                None
            }
            _ => None,
        };
        if let Some(error) = error {
            *errors.entry(error).or_insert(0_usize) += 1;
        }
    }
    eprintln!(
        "allocation fixture={} validator={validator:?} generation={} version={version} frontier={frontier} allocation(round, locked, qc)={allocation:?} tasks(succeeded, cancelled, voting)={tasks:?} errors={errors:?}",
        base.display(),
        snapshot.generation
    );
}
