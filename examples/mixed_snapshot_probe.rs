//! Private local acceptance helper, not a public node API. Output contains business state.
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use second::{StateRecoveryPayload, StateStore};
use serde_json::json;

fn main() {
    let base = std::env::args().nth(1).expect("snapshot base");
    let store = StateStore::new(base);
    let snapshot = store.load().unwrap().unwrap();
    if std::env::args().nth(2).as_deref() == Some("--bft") {
        let task = second::TaskId::parse(&std::env::args().nth(3).expect("task ID")).unwrap();
        println!(
            "generation={} frontier={} task={task:?} succeeded={:?} cancelled={} owned={}",
            snapshot.generation,
            snapshot.state.next_currency_address(),
            snapshot.state.task_succeeded(task.clone()),
            snapshot.state.task_cancelled(task.clone()),
            second::PreparedTaskBook::new(store.clone())
                .unwrap()
                .is_prepared(task.clone())
        );
        let scopes = [
            second::ConsensusScope::PreparedTask(task),
            second::ConsensusScope::CurrencyAllocation {
                validator_set_version: snapshot.validator_set.version(),
                start: snapshot.state.next_currency_address(),
            },
        ];
        for credential in snapshot.validator_set.credentials() {
            let validator = credential.id();
            for scope in &scopes {
                if let Some(local) = store.bft_local_state(validator, scope).unwrap() {
                    println!(
                        "validator={validator:?} scope={scope:?} round={} lock={:?} locked_digest={:?} highest_prevote_qc={:?}",
                        local.round(),
                        local.locked_round(),
                        local.locked_digest(),
                        local.valid_prevote_qc().map(|qc| qc.statement())
                    );
                }
            }
        }
        return;
    }
    let payload = StateRecoveryPayload::from_persisted(&snapshot)
        .unwrap()
        .encode_bytes()
        .unwrap();
    println!(
        "{}",
        json!({
            "payload": STANDARD.encode(payload),
            "safety_ready": snapshot.validator_safety_ready,
            "minimum_signing_version": snapshot.minimum_signing_validator_set_version,
            "recovery_proof": snapshot.recovery_checkpoint_proof.is_some()
        })
    );
}
