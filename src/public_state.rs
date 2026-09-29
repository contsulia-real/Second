use sha2::{Digest, Sha256};

use crate::{CurrencyRole, SecondState};

const PUBLIC_CURRENCY_STATE_DOMAIN: &[u8] = b"SECOND_PUBLIC_CURRENCY_STATE_V1\0";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicCurrencySummary {
    pub next_currency_address: u64,
    pub current_supply: u64,
    pub reserve_count: u64,
    pub occupied_count: u64,
    pub state_digest: [u8; 32],
}

pub(crate) fn summarize_public_currency_state(state: &SecondState) -> PublicCurrencySummary {
    let current_supply = state.business.currencies.len() as u64;
    let reserve_count = state
        .business
        .currencies
        .values()
        .filter(|currency| currency.role == CurrencyRole::Reserve && currency.owner.is_none())
        .count() as u64;
    let occupied_count = state
        .business
        .currencies
        .values()
        .filter(|currency| currency.owner.is_some())
        .count() as u64;

    let mut hasher = Sha256::new();
    hasher.update(PUBLIC_CURRENCY_STATE_DOMAIN);
    hasher.update(state.protocol.next_currency_address.to_be_bytes());
    hasher.update(current_supply.to_be_bytes());
    hasher.update(reserve_count.to_be_bytes());
    hasher.update(occupied_count.to_be_bytes());

    for currency in state.business.currencies.values() {
        hasher.update(currency.address.value().to_be_bytes());
        hasher.update([1]);
        hasher.update([u8::from(currency.owner.is_some())]);
        hasher.update([match currency.role {
            CurrencyRole::Circulation => 1,
            CurrencyRole::Reserve => 2,
        }]);
    }

    PublicCurrencySummary {
        next_currency_address: state.protocol.next_currency_address,
        current_supply,
        reserve_count,
        occupied_count,
        state_digest: hasher.finalize().into(),
    }
}
