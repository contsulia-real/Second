//! Shared interval splitting/merging for private state, claims and public deltas.
use crate::{AddressRange, CurrencyAddress};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RangeMap<T>(BTreeMap<u64, (u64, T)>);

impl<T> Default for RangeMap<T> {
    fn default() -> Self {
        Self(BTreeMap::new())
    }
}

impl<T: Clone + Eq> RangeMap<T> {
    pub(crate) fn runs(&self) -> impl Iterator<Item = (AddressRange, &T)> {
        self.0.iter().map(|(start, (len, value))| {
            (
                AddressRange {
                    start: CurrencyAddress::new(*start),
                    len: *len,
                },
                value,
            )
        })
    }
    pub(crate) fn get(&self, address: CurrencyAddress) -> Option<&T> {
        let (start, (len, value)) = self.0.range(..=address.value()).next_back()?;
        (address.value() - start < *len).then_some(value)
    }
    pub(crate) fn overlapping(&self, range: AddressRange) -> Vec<(AddressRange, T)> {
        let begin = self
            .0
            .range(..=range.start.value())
            .next_back()
            .map(|(s, _)| *s)
            .unwrap_or(range.start.value());
        self.0
            .range(begin..range.end())
            .filter_map(|(start, (len, value))| {
                let left = (*start).max(range.start.value());
                let end = (*start + *len).min(range.end());
                (left < end).then(|| {
                    (
                        AddressRange {
                            start: CurrencyAddress::new(left),
                            len: end - left,
                        },
                        value.clone(),
                    )
                })
            })
            .collect()
    }
    fn split(&mut self, at: u64) {
        let Some((start, (len, value))) = self.0.range(..at).next_back() else {
            return;
        };
        let end = *start + *len;
        if end <= at {
            return;
        }
        let start = *start;
        let value = value.clone();
        self.0.get_mut(&start).unwrap().0 = at - start;
        self.0.insert(at, (end - at, value));
    }
    pub(crate) fn set(&mut self, range: AddressRange, value: Option<T>) {
        assert!(AddressRange::new(range.start, range.len).is_some());
        let start = range.start.value();
        let end = range.end();
        self.split(start);
        self.split(end);
        let keys = self
            .0
            .range(start..end)
            .map(|(s, _)| *s)
            .collect::<Vec<_>>();
        for key in keys {
            self.0.remove(&key);
        }
        if let Some(value) = value {
            let mut left = start;
            let mut right = end;
            if let Some((s, (len, previous))) = self.0.range(..start).next_back()
                && *s + *len == start
                && previous == &value
            {
                left = *s;
            }
            if let Some((len, next)) = self.0.get(&end)
                && next == &value
            {
                right += *len;
                self.0.remove(&end);
            }
            self.0.insert(left, (right - left, value));
        }
    }
    pub(crate) fn append(&mut self, range: AddressRange, value: T) -> Result<(), ()> {
        if AddressRange::new(range.start, range.len).is_none() {
            return Err(());
        }
        if let Some((start, (len, old))) = self.0.last_key_value()
            && (*start + *len > range.start.value()
                || (*start + *len == range.start.value() && old == &value))
        {
            return Err(());
        }
        self.0.insert(range.start.value(), (range.len, value));
        Ok(())
    }
}
