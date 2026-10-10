use std::collections::BTreeSet;

use crate::AddressRanges;
use crate::state::{BusinessState, SecondState};
use crate::{AccountAddress, CurrencyAddress, CurrencyRole, ExecutionError};

impl SecondState {
    pub(crate) fn claim_transfer_candidates(
        &self,
        working: &mut BusinessState,
        source: AccountAddress,
        destination: AccountAddress,
        candidates: &AddressRanges,
    ) -> Result<(), ExecutionError> {
        for range in candidates.ranges() {
            let mut cursor = range.start.value();
            for (part, currency) in working.currencies.scan(*range) {
                if part.start.value() != cursor {
                    return Err(ExecutionError::CurrencyNotFound(CurrencyAddress::new(
                        cursor,
                    )));
                }
                if currency.role != CurrencyRole::Circulation {
                    return Err(ExecutionError::CurrencyNotCirculation(part.start));
                }
                if currency.owner != Some(source) {
                    return Err(ExecutionError::CurrencyNotOwned(part.start));
                }
                cursor = part.end();
            }
            if cursor != range.end() {
                return Err(ExecutionError::CurrencyNotFound(CurrencyAddress::new(
                    cursor,
                )));
            }
        }
        for range in candidates.ranges() {
            working
                .currencies
                .set_range(*range, CurrencyRole::Circulation, Some(destination));
        }
        Ok(())
    }

    pub(crate) fn validate_destroy_targets(
        &self,
        working: &BusinessState,
        currencies: &[CurrencyAddress],
    ) -> Result<(), ExecutionError> {
        self.require_unique_currency_list(currencies)?;

        for address in currencies {
            let currency = working
                .currencies
                .get(address)
                .ok_or(ExecutionError::CurrencyNotFound(*address))?;

            if currency.role != CurrencyRole::Circulation {
                return Err(ExecutionError::CurrencyNotCirculation(*address));
            }
            if currency.owner.is_some() {
                return Err(ExecutionError::CurrencyStillOccupied(*address));
            }
        }

        Ok(())
    }

    pub(crate) fn apply_destroy(
        &self,
        working: &mut BusinessState,
        currencies: &[CurrencyAddress],
    ) {
        for address in currencies {
            working.currencies.remove(address);
        }
    }

    pub(crate) fn validate_leaked_owners(
        &self,
        working: &BusinessState,
        leaked: &[CurrencyAddress],
    ) -> Result<Vec<AccountAddress>, ExecutionError> {
        self.require_unique_currency_list(leaked)?;

        leaked
            .iter()
            .map(|address| {
                let currency = working
                    .currencies
                    .get(address)
                    .ok_or(ExecutionError::CurrencyNotFound(*address))?;

                if currency.role != CurrencyRole::Circulation {
                    return Err(ExecutionError::CurrencyNotCirculation(*address));
                }

                currency
                    .owner
                    .ok_or(ExecutionError::CurrencyNotOwned(*address))
            })
            .collect()
    }

    pub(crate) fn apply_leak_repair_preallocated(
        &self,
        working: &mut BusinessState,
        leaked: &[CurrencyAddress],
        leaked_owners: &[AccountAddress],
        reserve: &AddressRanges,
        replacement_reserve: &AddressRanges,
    ) -> Result<(), ExecutionError> {
        if leaked.len() != leaked_owners.len()
            || leaked.len() as u64 != reserve.len()
            || leaked.len() as u64 != replacement_reserve.len()
        {
            return Err(ExecutionError::ReserveUnavailable {
                required: leaked.len() as u64,
                available: reserve.len(),
            });
        }

        self.require_unique_currency_list(leaked)?;

        for (address, expected_owner) in leaked.iter().zip(leaked_owners) {
            let currency = working
                .currencies
                .get(address)
                .ok_or(ExecutionError::CurrencyNotFound(*address))?;
            if currency.role != CurrencyRole::Circulation {
                return Err(ExecutionError::CurrencyNotCirculation(*address));
            }
            if currency.owner != Some(*expected_owner) {
                return Err(ExecutionError::CurrencyNotOwned(*address));
            }
        }

        for address in reserve.addresses() {
            let currency = working
                .currencies
                .get(&address)
                .ok_or(ExecutionError::CurrencyNotFound(address))?;
            if currency.role != CurrencyRole::Reserve || currency.owner.is_some() {
                return Err(ExecutionError::CurrencyNotCirculation(address));
            }
        }

        for range in replacement_reserve.ranges() {
            if let Some((part, _)) = working.currencies.scan(*range).next() {
                return Err(ExecutionError::DuplicateCurrency(part.start));
            }
        }

        for ((leaked_address, reserve_address), owner) in
            leaked.iter().zip(reserve.addresses()).zip(leaked_owners)
        {
            working.currencies.remove(leaked_address);

            working
                .currencies
                .set_role(reserve_address, CurrencyRole::Circulation);
            working.currencies.set_owner(reserve_address, Some(*owner));
        }

        for range in replacement_reserve.ranges() {
            working
                .currencies
                .set_range(*range, CurrencyRole::Reserve, None);
        }

        Ok(())
    }

    pub(crate) fn apply_issue_preallocated(
        &self,
        working: &mut BusinessState,
        account: AccountAddress,
        addresses: &AddressRanges,
    ) -> Result<(), ExecutionError> {
        self.require_account(working, account)?;
        for range in addresses.ranges() {
            if let Some((part, _)) = working.currencies.scan(*range).next() {
                return Err(ExecutionError::DuplicateCurrency(part.start));
            }
        }
        for range in addresses.ranges() {
            working
                .currencies
                .set_range(*range, CurrencyRole::Circulation, Some(account));
        }
        Ok(())
    }

    pub(crate) fn require_unique_currency_list(
        &self,
        currencies: &[CurrencyAddress],
    ) -> Result<(), ExecutionError> {
        let mut seen = BTreeSet::new();
        for address in currencies {
            if !seen.insert(*address) {
                return Err(ExecutionError::DuplicateCurrency(*address));
            }
        }
        Ok(())
    }
}
