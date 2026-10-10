use super::*;
use std::collections::BTreeMap;
#[path = "test_support.rs"]
mod test_support;

#[test]
fn mutations_match_address_reference_and_canonical_form() {
    const WINDOW: u64 = 512;
    for (base, seed) in [
        (0, 17_u64),
        (0, 83),
        (u64::MAX - WINDOW, 17),
        (u64::MAX - WINDOW, 83),
    ] {
        let mut ledger = CurrencyLedger::new();
        let mut reference = test_support::ReferenceLedger::new();
        let mut random = seed;
        for step in 0..2000 {
            random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
            let offset = (random >> 32) % WINDOW;
            let address = CurrencyAddress::new(base + offset);
            let range = AddressRange::new(address, 1 + (random >> 24) % (WINDOW - offset)).unwrap();
            let role = if random & 8 == 0 {
                CurrencyRole::Reserve
            } else {
                CurrencyRole::Circulation
            };
            let owner = (role == CurrencyRole::Circulation && random & 16 != 0)
                .then(|| AccountAddress::from_bytes([(random >> 16) as u8 % 3; 32]));
            let run = CurrencyRun {
                len: range.len,
                role,
                owner,
            };
            match (random >> 8) % 6 {
                0 => {
                    assert_eq!(ledger.remove(&address), reference.remove(&address));
                }
                1 => {
                    let currency = Currency {
                        address,
                        role,
                        owner,
                    };
                    assert_eq!(
                        ledger.insert(address, currency.clone()),
                        reference.insert(address, currency)
                    );
                }
                2 => {
                    ledger.set_owner(address, owner);
                    if let Some(currency) = reference.get_mut(&address) {
                        currency.owner = owner;
                    }
                }
                3 => {
                    ledger.set_role(address, role);
                    if let Some(currency) = reference.get_mut(&address) {
                        currency.role = role;
                    }
                }
                4 => {
                    ledger.set_range(range, role, owner);
                    test_support::set_range(&mut reference, range, run);
                }
                _ => {
                    let allowed = reference.last_key_value().is_none_or(|(last, currency)| {
                        last.value() < address.value()
                            && (last.value() + 1 != address.value()
                                || currency.role != role
                                || currency.owner != owner)
                    });
                    assert_eq!(ledger.append_run(address, run).is_ok(), allowed);
                    if allowed {
                        test_support::set_range(&mut reference, range, run);
                    }
                }
            }
            assert_eq!(
                ledger.iter().collect::<BTreeMap<_, _>>(),
                reference,
                "base={base} seed={seed} step={step}"
            );
            let canonical = test_support::runs(&reference);
            assert_eq!(ledger.runs().collect::<Vec<_>>(), canonical);

            let scanned = reference
                .range(range.start..CurrencyAddress::new(range.end()))
                .map(|(address, currency)| (*address, currency.clone()))
                .collect();
            assert_eq!(
                ledger
                    .scan(range)
                    .map(|(part, run)| {
                        assert_eq!(part.len, run.len);
                        (part.start, run)
                    })
                    .collect::<Vec<_>>(),
                test_support::runs(&scanned)
            );

            // Snapshot decoding and reverse insertion must recover the same canonical state.
            let mut decoded = CurrencyLedger::new();
            for (start, run) in canonical {
                decoded.append_run(start, run).unwrap();
            }
            assert_eq!(ledger, decoded);
            if step % 32 == 0 {
                let mut rebuilt = CurrencyLedger::new();
                for (address, currency) in reference.iter().rev() {
                    rebuilt.insert(*address, currency.clone());
                }
                assert_eq!(ledger, rebuilt);
            }
        }
    }
}
