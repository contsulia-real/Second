//! Canonical private currency runs; addresses keep their individual identity.
use crate::AddressRange;
use crate::range_map::RangeMap;

use crate::currency::Currency;
use crate::{AccountAddress, CurrencyAddress, CurrencyRole};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CurrencyRun {
    pub(crate) len: u64,
    pub(crate) role: CurrencyRole,
    pub(crate) owner: Option<AccountAddress>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct CurrencyLedger {
    runs: RangeMap<(CurrencyRole, Option<AccountAddress>)>,
}

impl CurrencyLedger {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn runs(&self) -> impl Iterator<Item = (CurrencyAddress, CurrencyRun)> + '_ {
        self.runs.runs().map(|(range, (role, owner))| {
            (
                range.start,
                CurrencyRun {
                    len: range.len,
                    role: *role,
                    owner: *owner,
                },
            )
        })
    }

    pub(crate) fn run_count(&self) -> usize {
        self.runs.runs().count()
    }

    pub(crate) fn len(&self) -> u64 {
        self.runs().map(|(_, run)| run.len).sum()
    }

    pub(crate) fn owned_runs(
        &self,
        owner: AccountAddress,
    ) -> impl Iterator<Item = (CurrencyAddress, CurrencyRun)> + '_ {
        self.runs().filter(move |(_, run)| run.owner == Some(owner))
    }

    pub(crate) fn reserve_runs(&self) -> impl Iterator<Item = (CurrencyAddress, CurrencyRun)> + '_ {
        self.runs()
            .filter(|(_, run)| run.role == CurrencyRole::Reserve && run.owner.is_none())
    }

    pub(crate) fn get(&self, address: &CurrencyAddress) -> Option<Currency> {
        let (role, owner) = self.runs.get(*address)?;
        Some(Currency {
            address: *address,
            role: *role,
            owner: *owner,
        })
    }
    pub(crate) fn contains_key(&self, address: &CurrencyAddress) -> bool {
        self.get(address).is_some()
    }
    pub(crate) fn insert(
        &mut self,
        address: CurrencyAddress,
        currency: Currency,
    ) -> Option<Currency> {
        assert_eq!(address, currency.address);
        let old = self.get(&address);
        self.runs.set(
            AddressRange::new(address, 1).expect("allocatable address"),
            Some((currency.role, currency.owner)),
        );
        old
    }
    pub(crate) fn remove(&mut self, address: &CurrencyAddress) -> Option<Currency> {
        let old = self.get(address)?;
        self.runs.set(AddressRange::new(*address, 1).unwrap(), None);
        Some(old)
    }
    pub(crate) fn set_owner(&mut self, address: CurrencyAddress, owner: Option<AccountAddress>) {
        if let Some(mut currency) = self.get(&address) {
            currency.owner = owner;
            self.insert(address, currency);
        }
    }
    pub(crate) fn set_role(&mut self, address: CurrencyAddress, role: CurrencyRole) {
        if let Some(mut currency) = self.get(&address) {
            currency.role = role;
            self.insert(address, currency);
        }
    }
    pub(crate) fn set_range(
        &mut self,
        range: AddressRange,
        role: CurrencyRole,
        owner: Option<AccountAddress>,
    ) {
        self.runs.set(range, Some((role, owner)));
    }
    pub(crate) fn scan(
        &self,
        range: AddressRange,
    ) -> impl Iterator<Item = (AddressRange, CurrencyRun)> {
        self.runs
            .overlapping(range)
            .into_iter()
            .map(|(range, (role, owner))| {
                (
                    range,
                    CurrencyRun {
                        len: range.len,
                        role,
                        owner,
                    },
                )
            })
    }
    pub(crate) fn append_run(
        &mut self,
        start: CurrencyAddress,
        run: CurrencyRun,
    ) -> Result<(), ()> {
        self.runs.append(
            AddressRange::new(start, run.len).ok_or(())?,
            (run.role, run.owner),
        )
    }

    #[cfg(test)]
    pub(crate) fn iter(&self) -> impl Iterator<Item = (CurrencyAddress, Currency)> + '_ {
        self.runs().flat_map(move |(start, run)| {
            (start.value()..start.value() + run.len).map(move |value| {
                let address = CurrencyAddress::new(value);
                (
                    address,
                    Currency {
                        address,
                        role: run.role,
                        owner: run.owner,
                    },
                )
            })
        })
    }
}

#[cfg(test)]
mod tests;
