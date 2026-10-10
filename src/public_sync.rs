use crate::range_map::RangeMap;
use crate::{AddressRange, AddressRanges};

use crate::public_state_codec::{
    PUBLIC_CURRENCY_STATE_ENCODED_SIZE, PUBLIC_CURRENCY_SUMMARY_ENCODED_SIZE,
    decode_public_currency_state, decode_public_currency_summary, encode_public_currency_state,
    encode_public_currency_summary,
};
use crate::{
    CurrencyAddress, PublicCurrencyState, PublicCurrencySummary, PublicCurrencyView,
    PublicStateError,
};

pub const MAX_PUBLIC_CURRENCY_DELTA_SIZE: usize = 56 * 1024;
pub const MAX_PUBLIC_CURRENCY_DELTA_CHANGES: usize = 4_096;
const DELTA_VERSION: u32 = 1;
const DELTA_FIXED_SIZE: usize = 4 + 8 + 32 + 8 + PUBLIC_CURRENCY_SUMMARY_ENCODED_SIZE + 4;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PublicCurrencyDeltaChange {
    Upsert(PublicCurrencyState),
    Retire(AddressRange),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicCurrencyDelta {
    from_epoch: u64,
    from_state_digest: [u8; 32],
    to_epoch: u64,
    summary: PublicCurrencySummary,
    changes: Vec<PublicCurrencyDeltaChange>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PublicCurrencyDeltaError {
    WrongBaseEpoch { expected: u64, actual: u64 },
    WrongBaseDigest,
    NonIncreasingEpoch { from: u64, to: u64 },
    TooManyChanges { actual: usize, maximum: usize },
    DuplicateAddress(CurrencyAddress),
    InvalidView(PublicStateError),
    InvalidEncoding,
    TooLarge,
}

impl PublicCurrencyDelta {
    pub fn new(
        from_epoch: u64,
        from_state_digest: [u8; 32],
        to_epoch: u64,
        summary: PublicCurrencySummary,
        changes: Vec<PublicCurrencyDeltaChange>,
    ) -> Result<Self, PublicCurrencyDeltaError> {
        if to_epoch <= from_epoch {
            return Err(PublicCurrencyDeltaError::NonIncreasingEpoch {
                from: from_epoch,
                to: to_epoch,
            });
        }
        if changes.len() > MAX_PUBLIC_CURRENCY_DELTA_CHANGES {
            return Err(PublicCurrencyDeltaError::TooManyChanges {
                actual: changes.len(),
                maximum: MAX_PUBLIC_CURRENCY_DELTA_CHANGES,
            });
        }
        let mut end = None;
        for change in &changes {
            let range = change
                .range()
                .ok_or(PublicCurrencyDeltaError::InvalidEncoding)?;
            if range.end() > summary.next_currency_address {
                return Err(PublicCurrencyDeltaError::InvalidEncoding);
            }
            if end.is_some_and(|end| end > range.start.value()) {
                return Err(PublicCurrencyDeltaError::DuplicateAddress(range.start));
            }
            end = Some(range.end());
        }
        Ok(Self {
            from_epoch,
            from_state_digest,
            to_epoch,
            summary,
            changes,
        })
    }

    pub const fn from_epoch(&self) -> u64 {
        self.from_epoch
    }

    pub const fn from_state_digest(&self) -> [u8; 32] {
        self.from_state_digest
    }

    pub const fn to_epoch(&self) -> u64 {
        self.to_epoch
    }

    pub fn summary(&self) -> &PublicCurrencySummary {
        &self.summary
    }

    pub fn changes(&self) -> &[PublicCurrencyDeltaChange] {
        &self.changes
    }

    pub fn apply(
        &self,
        base_epoch: u64,
        base: &PublicCurrencyView,
    ) -> Result<PublicCurrencyView, PublicCurrencyDeltaError> {
        if base_epoch != self.from_epoch {
            return Err(PublicCurrencyDeltaError::WrongBaseEpoch {
                expected: self.from_epoch,
                actual: base_epoch,
            });
        }
        if base.summary.state_digest != self.from_state_digest {
            return Err(PublicCurrencyDeltaError::WrongBaseDigest);
        }

        if self.summary.next_currency_address < base.summary.next_currency_address {
            return Err(PublicCurrencyDeltaError::InvalidEncoding);
        }
        let mut states = RangeMap::default();
        for state in &base.states {
            states
                .append(
                    state
                        .range()
                        .ok_or(PublicCurrencyDeltaError::InvalidEncoding)?,
                    state.occupied,
                )
                .map_err(|_| PublicCurrencyDeltaError::InvalidEncoding)?;
        }
        for change in &self.changes {
            match change {
                PublicCurrencyDeltaChange::Upsert(state) => states.set(
                    state
                        .range()
                        .ok_or(PublicCurrencyDeltaError::InvalidEncoding)?,
                    Some(state.occupied),
                ),
                PublicCurrencyDeltaChange::Retire(range) => states.set(*range, None),
            }
        }
        PublicCurrencyView::new(
            self.summary.clone(),
            states
                .runs()
                .map(|(range, occupied)| PublicCurrencyState {
                    start: range.start,
                    len: range.len,
                    occupied: *occupied,
                })
                .collect(),
        )
        .map_err(PublicCurrencyDeltaError::InvalidView)
    }

    pub(crate) fn between(
        from_epoch: u64,
        base: &PublicCurrencyView,
        to_epoch: u64,
        target: &PublicCurrencyView,
    ) -> Result<Self, PublicCurrencyDeltaError> {
        let mut old = RangeMap::default();
        let mut old_live = AddressRanges::default();
        let mut new_live = AddressRanges::default();
        for state in &base.states {
            let range = state
                .range()
                .ok_or(PublicCurrencyDeltaError::InvalidEncoding)?;
            old.append(range, state.occupied)
                .map_err(|_| PublicCurrencyDeltaError::InvalidEncoding)?;
            old_live.insert(range);
        }
        let mut changes = Vec::new();
        for state in &target.states {
            let range = state
                .range()
                .ok_or(PublicCurrencyDeltaError::InvalidEncoding)?;
            new_live.insert(range);
            let mut unchanged = AddressRanges::default();
            for (part, occupied) in old.overlapping(range) {
                if occupied == state.occupied {
                    unchanged.insert(part);
                }
            }
            let changed = AddressRanges::single(range.start, range.len)
                .unwrap()
                .difference(&unchanged);
            for part in changed.ranges() {
                changes.push(PublicCurrencyDeltaChange::Upsert(PublicCurrencyState {
                    start: part.start,
                    len: part.len,
                    occupied: state.occupied,
                }));
            }
        }
        for range in old_live.difference(&new_live).ranges() {
            changes.push(PublicCurrencyDeltaChange::Retire(*range));
        }
        changes.sort_by_key(|change| change.range().unwrap().start);
        Self::new(
            from_epoch,
            base.summary.state_digest,
            to_epoch,
            target.summary.clone(),
            changes,
        )
    }

    pub fn encode_bytes(&self) -> Result<Vec<u8>, PublicCurrencyDeltaError> {
        let change_count =
            u32::try_from(self.changes.len()).map_err(|_| PublicCurrencyDeltaError::TooLarge)?;
        let mut out = Vec::with_capacity(DELTA_FIXED_SIZE + self.changes.len() * 18);
        out.extend_from_slice(&DELTA_VERSION.to_be_bytes());
        out.extend_from_slice(&self.from_epoch.to_be_bytes());
        out.extend_from_slice(&self.from_state_digest);
        out.extend_from_slice(&self.to_epoch.to_be_bytes());
        encode_public_currency_summary(&mut out, &self.summary);
        out.extend_from_slice(&change_count.to_be_bytes());
        for change in &self.changes {
            match change {
                PublicCurrencyDeltaChange::Upsert(state) => {
                    out.push(1);
                    encode_public_currency_state(&mut out, state);
                }
                PublicCurrencyDeltaChange::Retire(range) => {
                    out.push(2);
                    out.extend_from_slice(&range.start.value().to_be_bytes());
                    out.extend_from_slice(&range.len.to_be_bytes());
                }
            }
        }
        if out.len() > MAX_PUBLIC_CURRENCY_DELTA_SIZE {
            return Err(PublicCurrencyDeltaError::TooLarge);
        }
        Ok(out)
    }

    pub fn decode_bytes(bytes: &[u8]) -> Result<Self, PublicCurrencyDeltaError> {
        if bytes.len() > MAX_PUBLIC_CURRENCY_DELTA_SIZE {
            return Err(PublicCurrencyDeltaError::TooLarge);
        }
        let mut decoder = Decoder::new(bytes);
        if decoder.u32()? != DELTA_VERSION {
            return Err(PublicCurrencyDeltaError::InvalidEncoding);
        }
        let from_epoch = decoder.u64()?;
        let from_state_digest = decoder.array_32()?;
        let to_epoch = decoder.u64()?;
        let summary =
            decode_public_currency_summary(decoder.take(PUBLIC_CURRENCY_SUMMARY_ENCODED_SIZE)?)
                .map_err(|_| PublicCurrencyDeltaError::InvalidEncoding)?;
        let change_count = usize::try_from(decoder.u32()?)
            .map_err(|_| PublicCurrencyDeltaError::InvalidEncoding)?;
        if change_count > MAX_PUBLIC_CURRENCY_DELTA_CHANGES {
            return Err(PublicCurrencyDeltaError::TooManyChanges {
                actual: change_count,
                maximum: MAX_PUBLIC_CURRENCY_DELTA_CHANGES,
            });
        }

        let mut changes = Vec::with_capacity(change_count);
        for _ in 0..change_count {
            match decoder.u8()? {
                1 => changes.push(PublicCurrencyDeltaChange::Upsert(
                    decode_public_currency_state(decoder.take(PUBLIC_CURRENCY_STATE_ENCODED_SIZE)?)
                        .map_err(|_| PublicCurrencyDeltaError::InvalidEncoding)?,
                )),
                2 => changes.push(PublicCurrencyDeltaChange::Retire(
                    AddressRange::new(CurrencyAddress::new(decoder.u64()?), decoder.u64()?)
                        .ok_or(PublicCurrencyDeltaError::InvalidEncoding)?,
                )),
                _ => return Err(PublicCurrencyDeltaError::InvalidEncoding),
            }
        }
        if !decoder.finished() {
            return Err(PublicCurrencyDeltaError::InvalidEncoding);
        }
        Self::new(from_epoch, from_state_digest, to_epoch, summary, changes)
    }
}

struct Decoder<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Decoder<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], PublicCurrencyDeltaError> {
        let end = self
            .offset
            .checked_add(len)
            .ok_or(PublicCurrencyDeltaError::InvalidEncoding)?;
        let slice = self
            .bytes
            .get(self.offset..end)
            .ok_or(PublicCurrencyDeltaError::InvalidEncoding)?;
        self.offset = end;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8, PublicCurrencyDeltaError> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32, PublicCurrencyDeltaError> {
        Ok(u32::from_be_bytes(
            self.take(4)?
                .try_into()
                .map_err(|_| PublicCurrencyDeltaError::InvalidEncoding)?,
        ))
    }

    fn u64(&mut self) -> Result<u64, PublicCurrencyDeltaError> {
        Ok(u64::from_be_bytes(
            self.take(8)?
                .try_into()
                .map_err(|_| PublicCurrencyDeltaError::InvalidEncoding)?,
        ))
    }

    fn array_32(&mut self) -> Result<[u8; 32], PublicCurrencyDeltaError> {
        self.take(32)?
            .try_into()
            .map_err(|_| PublicCurrencyDeltaError::InvalidEncoding)
    }

    const fn finished(&self) -> bool {
        self.offset == self.bytes.len()
    }
}

impl PublicCurrencyDeltaChange {
    fn range(&self) -> Option<AddressRange> {
        match self {
            Self::Upsert(state) => state.range(),
            Self::Retire(range) => AddressRange::new(range.start, range.len),
        }
    }
}
