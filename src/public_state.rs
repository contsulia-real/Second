use sha2::{Digest, Sha256};

use crate::{CurrencyAddress, CurrencyRole, PublicCurrencyState, SecondState};

const PUBLIC_CURRENCY_STATE_DOMAIN: &[u8] = b"SECOND_PUBLIC_CURRENCY_STATE_V1\0";

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PublicStateError {
    StateAtOrBeyondFrontier {
        address: CurrencyAddress,
        frontier: u64,
    },
    NonIncreasingAddress {
        previous: CurrencyAddress,
        current: CurrencyAddress,
    },
    OccupiedReserve(CurrencyAddress),
    SummaryMismatch,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicCurrencyView {
    pub summary: PublicCurrencySummary,
    pub states: Vec<PublicCurrencyState>,
}

impl PublicCurrencyView {
    pub fn new(
        summary: PublicCurrencySummary,
        states: Vec<PublicCurrencyState>,
    ) -> Result<Self, PublicStateError> {
        validate_public_states(summary.next_currency_address, &states)?;
        let computed = summarize_public_states(summary.next_currency_address, &states);

        if computed != summary {
            return Err(PublicStateError::SummaryMismatch);
        }

        Ok(Self { summary, states })
    }
}

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

    let mut hasher = summary_hasher(
        state.protocol.next_currency_address,
        current_supply,
        reserve_count,
        occupied_count,
    );

    for currency in state.business.currencies.values() {
        hash_public_state(&mut hasher, &currency.public_state());
    }

    PublicCurrencySummary {
        next_currency_address: state.protocol.next_currency_address,
        current_supply,
        reserve_count,
        occupied_count,
        state_digest: hasher.finalize().into(),
    }
}

fn validate_public_states(
    next_currency_address: u64,
    states: &[PublicCurrencyState],
) -> Result<(), PublicStateError> {
    let mut previous = None;

    for state in states {
        if state.address.value() >= next_currency_address {
            return Err(PublicStateError::StateAtOrBeyondFrontier {
                address: state.address,
                frontier: next_currency_address,
            });
        }

        if state.role == CurrencyRole::Reserve && state.occupied {
            return Err(PublicStateError::OccupiedReserve(state.address));
        }

        if let Some(previous_address) = previous
            && state.address <= previous_address
        {
            return Err(PublicStateError::NonIncreasingAddress {
                previous: previous_address,
                current: state.address,
            });
        }

        previous = Some(state.address);
    }

    Ok(())
}

fn summarize_public_states(
    next_currency_address: u64,
    states: &[PublicCurrencyState],
) -> PublicCurrencySummary {
    let current_supply = states.len() as u64;
    let reserve_count = states
        .iter()
        .filter(|state| state.role == CurrencyRole::Reserve && !state.occupied)
        .count() as u64;
    let occupied_count = states.iter().filter(|state| state.occupied).count() as u64;

    let mut hasher = summary_hasher(
        next_currency_address,
        current_supply,
        reserve_count,
        occupied_count,
    );

    for state in states {
        hash_public_state(&mut hasher, state);
    }

    PublicCurrencySummary {
        next_currency_address,
        current_supply,
        reserve_count,
        occupied_count,
        state_digest: hasher.finalize().into(),
    }
}

fn summary_hasher(
    next_currency_address: u64,
    current_supply: u64,
    reserve_count: u64,
    occupied_count: u64,
) -> Sha256 {
    let mut hasher = Sha256::new();
    hasher.update(PUBLIC_CURRENCY_STATE_DOMAIN);
    hasher.update(next_currency_address.to_be_bytes());
    hasher.update(current_supply.to_be_bytes());
    hasher.update(reserve_count.to_be_bytes());
    hasher.update(occupied_count.to_be_bytes());
    hasher
}

fn hash_public_state(hasher: &mut Sha256, state: &PublicCurrencyState) {
    hasher.update(state.address.value().to_be_bytes());
    hasher.update([u8::from(state.occupied)]);
    hasher.update([match state.role {
        CurrencyRole::Circulation => 1,
        CurrencyRole::Reserve => 2,
    }]);
}
