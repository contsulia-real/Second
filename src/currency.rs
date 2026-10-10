use crate::{AccountAddress, CurrencyAddress};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CurrencyRole {
    Circulation,
    Reserve,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicCurrencyState {
    pub start: CurrencyAddress,
    pub len: u64,
    pub occupied: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Currency {
    pub(crate) address: CurrencyAddress,
    pub(crate) role: CurrencyRole,
    pub(crate) owner: Option<AccountAddress>,
}

impl Currency {
    pub(crate) fn public_state(&self) -> PublicCurrencyState {
        PublicCurrencyState {
            start: self.address,
            len: 1,
            occupied: self.owner.is_some() || self.role == CurrencyRole::Reserve,
        }
    }
}

impl PublicCurrencyState {
    pub fn range(&self) -> Option<crate::AddressRange> {
        crate::AddressRange::new(self.start, self.len)
    }
}
