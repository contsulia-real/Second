use std::time::{SystemTime, UNIX_EPOCH};

use ed25519_dalek::SigningKey;

use super::*;
use crate::{SecondState, ValidatorCredential};

fn key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

fn credential(id: u64) -> ValidatorCredential {
    ValidatorCredential::new(
        ValidatorId::new(id),
        key((id * 3) as u8).verifying_key().to_bytes(),
        key((id * 3 + 1) as u8).verifying_key().to_bytes(),
        key((id * 3 + 2) as u8).verifying_key().to_bytes(),
    )
    .unwrap()
}

fn validator_set(version: u64) -> ValidatorSet {
    ValidatorSet::new(version, (1..=4).map(credential)).unwrap()
}

fn temp_store() -> (StateStore, std::path::PathBuf) {
    let unique = format!(
        "second-bft-rejected-cache-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock must be after unix epoch")
            .as_nanos(),
    );
    let base = std::env::temp_dir().join(unique);
    (StateStore::new(&base), base)
}

#[test]
fn rejected_nodes_survive_stable_authority_refresh_and_reset_on_authority_change() {
    let durable_set = validator_set(1);
    let (store, base) = temp_store();
    store
        .initialize(&SecondState::genesis([], 1), &durable_set)
        .unwrap();

    let runtime = ValidatorBftRuntime::new(
        ValidatorRuntimeKeys::new(ValidatorId::new(1), key(3), key(4)),
        store.clone(),
        durable_set,
        std::iter::empty(),
    )
    .unwrap();
    let rejected = NodeId::from_bytes([91; 32]);

    runtime.reject_node(rejected);
    runtime.refresh_authority().unwrap();
    assert!(
        runtime.rejected_node_ids().contains(&rejected),
        "unchanged durable authority must not reopen an unauthorized peer every maintenance cycle"
    );

    *runtime
        .inner
        .authority
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) =
        ValidatorBftAuthority::new(validator_set(2), std::iter::empty()).unwrap();

    runtime.refresh_authority().unwrap();
    assert!(
        !runtime.rejected_node_ids().contains(&rejected),
        "a real authority change must allow previously rejected nodes to be reevaluated"
    );

    drop(runtime);
    store.remove_files().unwrap();
    let _ = std::fs::remove_file(base.with_extension("lock"));
}
