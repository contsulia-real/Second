use super::CurrencyRun;
use crate::currency::Currency;
use crate::{AddressRange, CurrencyAddress};
use std::collections::BTreeMap;

pub(super) type ReferenceLedger = BTreeMap<CurrencyAddress, Currency>;

pub(super) fn set_range(reference: &mut ReferenceLedger, range: AddressRange, run: CurrencyRun) {
    for value in range.start.value()..range.end() {
        let address = CurrencyAddress::new(value);
        reference.insert(
            address,
            Currency {
                address,
                role: run.role,
                owner: run.owner,
            },
        );
    }
}

pub(super) fn runs(reference: &ReferenceLedger) -> Vec<(CurrencyAddress, CurrencyRun)> {
    let mut runs: Vec<(CurrencyAddress, CurrencyRun)> = Vec::new();
    for (address, currency) in reference {
        if let Some((start, run)) = runs.last_mut()
            && start.value() + run.len == address.value()
            && run.role == currency.role
            && run.owner == currency.owner
        {
            run.len += 1;
        } else {
            runs.push((
                *address,
                CurrencyRun {
                    len: 1,
                    role: currency.role,
                    owner: currency.owner,
                },
            ));
        }
    }
    runs
}
