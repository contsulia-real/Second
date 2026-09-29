use crate::{AccountAddress, CurrencyAddress};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CurrencyRole {
    Circulation,
    Reserve,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicCurrencyState {
    pub address: CurrencyAddress,
    pub exists: bool,
    pub occupied: bool,
    pub role: CurrencyRole,
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
            address: self.address,
            exists: true,
            occupied: self.owner.is_some(),
            role: self.role,
        }
    }
}
