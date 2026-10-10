use crate::{CurrencyAddress, PublicCurrencyState, PublicCurrencySummary};

pub(crate) const PUBLIC_CURRENCY_SUMMARY_ENCODED_SIZE: usize = 64;
pub(crate) const PUBLIC_CURRENCY_STATE_ENCODED_SIZE: usize = 17;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PublicStateCodecError {
    Length,
    Boolean(u8),
    InvalidRange,
}

pub(crate) fn encode_public_currency_summary(out: &mut Vec<u8>, summary: &PublicCurrencySummary) {
    out.extend_from_slice(&summary.next_currency_address.to_be_bytes());
    out.extend_from_slice(&summary.current_supply.to_be_bytes());
    out.extend_from_slice(&summary.reserve_count.to_be_bytes());
    out.extend_from_slice(&summary.occupied_count.to_be_bytes());
    out.extend_from_slice(&summary.state_digest);
}

pub(crate) fn decode_public_currency_summary(
    bytes: &[u8],
) -> Result<PublicCurrencySummary, PublicStateCodecError> {
    if bytes.len() != PUBLIC_CURRENCY_SUMMARY_ENCODED_SIZE {
        return Err(PublicStateCodecError::Length);
    }
    Ok(PublicCurrencySummary {
        next_currency_address: read_u64(&bytes[0..8])?,
        current_supply: read_u64(&bytes[8..16])?,
        reserve_count: read_u64(&bytes[16..24])?,
        occupied_count: read_u64(&bytes[24..32])?,
        state_digest: bytes[32..64]
            .try_into()
            .map_err(|_| PublicStateCodecError::Length)?,
    })
}

pub(crate) fn encode_public_currency_state(out: &mut Vec<u8>, state: &PublicCurrencyState) {
    out.extend_from_slice(&state.start.value().to_be_bytes());
    out.extend_from_slice(&state.len.to_be_bytes());
    out.push(u8::from(state.occupied));
}

pub(crate) fn decode_public_currency_state(
    bytes: &[u8],
) -> Result<PublicCurrencyState, PublicStateCodecError> {
    if bytes.len() != PUBLIC_CURRENCY_STATE_ENCODED_SIZE {
        return Err(PublicStateCodecError::Length);
    }
    let occupied = match bytes[16] {
        0 => false,
        1 => true,
        value => return Err(PublicStateCodecError::Boolean(value)),
    };
    let start = CurrencyAddress::new(read_u64(&bytes[0..8])?);
    let len = read_u64(&bytes[8..16])?;
    crate::AddressRange::new(start, len).ok_or(PublicStateCodecError::InvalidRange)?;
    Ok(PublicCurrencyState {
        start,
        len,
        occupied,
    })
}

fn read_u64(bytes: &[u8]) -> Result<u64, PublicStateCodecError> {
    Ok(u64::from_be_bytes(
        bytes
            .try_into()
            .map_err(|_| PublicStateCodecError::Length)?,
    ))
}
