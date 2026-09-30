use std::collections::BTreeSet;

use crate::currency::Currency;
use crate::state::{BusinessState, SecondState};
use crate::{AccountAddress, CurrencyAddress, CurrencyRole, ExecutionError};

impl SecondState {
    pub(crate) fn claim_transfer_candidates(
        &self,
        working: &mut BusinessState,
        source: AccountAddress,
        destination: AccountAddress,
        candidates: &[CurrencyAddress],
    ) -> Result<(), ExecutionError> {
        for address in candidates {
            let currency = working
                .currencies
                .get(address)
                .ok_or(ExecutionError::CurrencyNotFound(*address))?;

            if currency.role != CurrencyRole::Circulation {
                return Err(ExecutionError::CurrencyNotCirculation(*address));
            }
            if currency.owner != Some(source) {
                return Err(ExecutionError::CurrencyNotOwned(*address));
            }
        }

        for address in candidates {
            if let Some(currency) = working.currencies.get_mut(address) {
                currency.owner = Some(destination);
            }
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
        reserve: &[CurrencyAddress],
        replacement_reserve: &[CurrencyAddress],
    ) -> Result<(), ExecutionError> {
        if leaked.len() != leaked_owners.len()
            || leaked.len() != reserve.len()
            || leaked.len() != replacement_reserve.len()
        {
            return Err(ExecutionError::ReserveUnavailable {
                required: leaked.len() as u64,
                available: reserve.len() as u64,
            });
        }

        self.require_unique_currency_list(leaked)?;
        self.require_unique_currency_list(reserve)?;
        self.require_unique_currency_list(replacement_reserve)?;

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

        for address in reserve {
            let currency = working
                .currencies
                .get(address)
                .ok_or(ExecutionError::CurrencyNotFound(*address))?;
            if currency.role != CurrencyRole::Reserve || currency.owner.is_some() {
                return Err(ExecutionError::CurrencyNotCirculation(*address));
            }
        }

        for address in replacement_reserve {
            if working.currencies.contains_key(address) {
                return Err(ExecutionError::DuplicateCurrency(*address));
            }
        }

        for ((leaked_address, reserve_address), owner) in
            leaked.iter().zip(reserve.iter()).zip(leaked_owners)
        {
            working.currencies.remove(leaked_address);

            let reserve_currency = working
                .currencies
                .get_mut(reserve_address)
                .ok_or(ExecutionError::CurrencyNotFound(*reserve_address))?;
            reserve_currency.role = CurrencyRole::Circulation;
            reserve_currency.owner = Some(*owner);
        }

        for address in replacement_reserve {
            working.currencies.insert(
                *address,
                Currency {
                    address: *address,
                    role: CurrencyRole::Reserve,
                    owner: None,
                },
            );
        }

        Ok(())
    }

    pub(crate) fn apply_issue_preallocated(
        &self,
        working: &mut BusinessState,
        account: AccountAddress,
        addresses: &[CurrencyAddress],
    ) -> Result<(), ExecutionError> {
        self.require_account(working, account)?;
        self.require_unique_currency_list(addresses)?;

        for address in addresses {
            if working.currencies.contains_key(address) {
                return Err(ExecutionError::DuplicateCurrency(*address));
            }
        }

        for address in addresses {
            working.currencies.insert(
                *address,
                Currency {
                    address: *address,
                    role: CurrencyRole::Circulation,
                    owner: Some(account),
                },
            );
        }

        Ok(())
    }

    pub(crate) fn require_account(
        &self,
        working: &BusinessState,
        account: AccountAddress,
    ) -> Result<(), ExecutionError> {
        if working.accounts.contains(&account) {
            Ok(())
        } else {
            Err(ExecutionError::AccountNotFound(account))
        }
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
