use std::time::{Duration, Instant};

use super::*;
use crate::{AddressRange, CurrencyClaimBook, CurrencyRole, OperationClaimId, SecondState, TaskId};

fn seed(index: u64) -> ReserveSamplingSeed {
    ReserveSamplingSeed::new(Sha256::digest(index.to_be_bytes()).into(), 0)
}

#[test]
fn fragmented_claim_pool_is_uniform_stable_and_sampled_without_replacement() {
    let mut state = SecondState::genesis([], 200);
    for (start, len) in [(1, 2), (10, 3), (50, 2), (100, 2)] {
        state.business.currencies.set_range(
            AddressRange::new(CurrencyAddress::new(start), len).unwrap(),
            CurrencyRole::Reserve,
            None,
        );
    }
    let mut claims = CurrencyClaimBook::new();
    claims
        .restore_leak_repair(
            OperationClaimId::new(TaskId::parse("other-task").unwrap(), 0),
            &[CurrencyAddress::new(160), CurrencyAddress::new(161)],
            &[CurrencyAddress::new(10), CurrencyAddress::new(50)]
                .into_iter()
                .collect(),
        )
        .unwrap();
    let task = TaskId::parse("sampling-task").unwrap();
    let available = claims
        .clone()
        .claim_leak_repair_in_business_state(
            OperationClaimId::new(task, 0),
            &state.business,
            &(150..157).map(CurrencyAddress::new).collect::<Vec<_>>(),
            &seed(0),
        )
        .unwrap();
    let addresses = [1, 2, 11, 12, 51, 100, 101];
    assert_eq!(
        available.addresses().map(|a| a.value()).collect::<Vec<_>>(),
        addresses
    );
    let reduced =
        available.difference(&AddressRanges::single(CurrencyAddress::new(101), 1).unwrap());
    // Fixed before measuring: Hoeffding + union bound < 1.7e-7 for a deviation of 800.
    const TRIALS: u64 = 70_000;
    const TOLERANCE: u64 = 800;
    let mut frequencies = [0_u64; 7];
    let mut changed_unrelated = 0;
    let mut unrelated = 0;
    for index in 0..TRIALS {
        let key = seed(index);
        let selected = key
            .select(&available, 1)
            .addresses()
            .next()
            .unwrap()
            .value();
        frequencies[addresses.binary_search(&selected).unwrap()] += 1;
        if selected != 101 {
            unrelated += 1;
            let after = key.select(&reduced, 1).addresses().next().unwrap().value();
            changed_unrelated += u64::from(selected != after);
        }
    }
    for frequency in frequencies {
        assert!(
            frequency.abs_diff(TRIALS / 7) <= TOLERANCE,
            "{frequencies:?}"
        );
    }
    // Conditional on not removing the selected address, the fixed fixture allows 1/4 changes.
    assert!(changed_unrelated * 4 <= unrelated);
    println!("uniformity: {frequencies:?}; unrelated removal: {changed_unrelated}/{unrelated}");
    for index in 0..128 {
        let selected = seed(index).select(&available, 7);
        assert_eq!(selected, available);
        let partial = seed(index).select(&available, 5);
        assert_eq!(partial.len(), 5);
        assert_eq!(partial.difference(&available).len(), 0);
    }
    // The fixed tree also covers both ends of the full 64-bit address space.
    let edge = [CurrencyAddress::new(0), CurrencyAddress::new(u64::MAX - 1)]
        .into_iter()
        .collect::<AddressRanges>();
    assert_eq!(seed(0).select(&edge, 2), edge);
}

#[test]
fn maximum_prepare_reserve_selection_is_bounded_by_ranges_and_draws() {
    let count = crate::MAX_LEAK_REPAIR_ADDRESSES_PER_TASK as u64;
    let leaked = (0..count).map(CurrencyAddress::new).collect::<Vec<_>>();
    for pool in [1_000_000, 1_000_000_000] {
        let mut state = SecondState::genesis([], pool + count);
        state.business.currencies.set_range(
            AddressRange::new(CurrencyAddress::new(count), pool).unwrap(),
            CurrencyRole::Reserve,
            None,
        );
        let mut claims = CurrencyClaimBook::new();
        let started = Instant::now();
        let selected = claims
            .claim_leak_repair_in_business_state(
                OperationClaimId::new(TaskId::parse("maximum-sampling").unwrap(), 0),
                &state.business,
                &leaked,
                &seed(pool),
            )
            .unwrap();
        let elapsed = started.elapsed();
        assert_eq!(selected.len(), count);
        assert_eq!(claims.claimed_currency_count(), count * 2);
        println!("Prepare Reserve selection: N={pool}, q={count}, elapsed={elapsed:?}");
        // The time gate is asserted only in release: cargo test --release --lib reserve_sampling::tests.
        if !cfg!(debug_assertions) {
            assert!(
                elapsed < Duration::from_secs(1),
                "performance gate: {elapsed:?}"
            );
        }
    }
}
