use sha2::{Digest, Sha256};

use crate::{CurrencyAddress, PublicCurrencyState, SecondState};

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
    InvalidRange(CurrencyAddress),
    NonCanonicalRange(CurrencyAddress),
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
        let computed = summarize_public_states(
            summary.next_currency_address,
            summary.reserve_count,
            &states,
        );

        if summary.reserve_count > summary.occupied_count || computed != summary {
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

pub(crate) fn public_states(state: &SecondState) -> Vec<PublicCurrencyState> {
    let mut states: Vec<PublicCurrencyState> = Vec::new();
    for (start, run) in state.business.currencies.runs() {
        let mut public = crate::currency::Currency {
            address: start,
            role: run.role,
            owner: run.owner,
        }
        .public_state();
        public.len = run.len;
        if let Some(previous) = states.last_mut()
            && previous.start.value() + previous.len == start.value()
            && previous.occupied == public.occupied
        {
            previous.len += public.len;
        } else {
            states.push(public);
        }
    }
    states
}

pub(crate) fn summarize_public_currency_state(state: &SecondState) -> PublicCurrencySummary {
    summarize_public_states(
        state.next_currency_address(),
        state.reserve_count(),
        &public_states(state),
    )
}

pub(crate) fn page(
    states: &[PublicCurrencyState],
    start: CurrencyAddress,
    limit: u16,
) -> crate::PublicCurrencyPage {
    let first = states.partition_point(|state| state.start.value() + state.len <= start.value());
    let end = (first + usize::from(limit)).min(states.len());
    let mut selected = states[first..end].to_vec();
    if let Some(head) = selected.first_mut()
        && head.start < start
    {
        head.len -= start.value() - head.start.value();
        head.start = start;
    }
    crate::PublicCurrencyPage {
        states: selected,
        next_start: states.get(end).map(|state| state.start),
    }
}

fn validate_public_states(
    next_currency_address: u64,
    states: &[PublicCurrencyState],
) -> Result<(), PublicStateError> {
    let mut previous: Option<&PublicCurrencyState> = None;
    for state in states {
        let range = state
            .range()
            .ok_or(PublicStateError::InvalidRange(state.start))?;
        if range.end() > next_currency_address {
            return Err(PublicStateError::StateAtOrBeyondFrontier {
                address: state.start,
                frontier: next_currency_address,
            });
        }
        if let Some(previous) = previous {
            let end = previous.start.value() + previous.len;
            if end > state.start.value() {
                return Err(PublicStateError::NonIncreasingAddress {
                    previous: previous.start,
                    current: state.start,
                });
            }
            if end == state.start.value() && previous.occupied == state.occupied {
                return Err(PublicStateError::NonCanonicalRange(state.start));
            }
        }
        previous = Some(state);
    }

    Ok(())
}

fn summarize_public_states(
    next_currency_address: u64,
    reserve_count: u64,
    states: &[PublicCurrencyState],
) -> PublicCurrencySummary {
    let current_supply = states.iter().map(|state| state.len).sum();
    let occupied_count = states
        .iter()
        .filter(|state| state.occupied)
        .map(|state| state.len)
        .sum();

    let mut hasher = summary_hasher(next_currency_address, current_supply, occupied_count);

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

fn summary_hasher(next_currency_address: u64, current_supply: u64, occupied_count: u64) -> Sha256 {
    let mut hasher = Sha256::new();
    hasher.update(PUBLIC_CURRENCY_STATE_DOMAIN);
    hasher.update(next_currency_address.to_be_bytes());
    hasher.update(current_supply.to_be_bytes());
    hasher.update(occupied_count.to_be_bytes());
    hasher
}

fn hash_public_state(hasher: &mut Sha256, state: &PublicCurrencyState) {
    hasher.update(state.start.value().to_be_bytes());
    hasher.update([u8::from(state.occupied)]);
    hasher.update(state.len.to_be_bytes());
}
