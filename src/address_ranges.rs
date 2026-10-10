use crate::CurrencyAddress;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AddressRange {
    pub start: CurrencyAddress,
    pub len: u64,
}

impl AddressRange {
    pub fn new(start: CurrencyAddress, len: u64) -> Option<Self> {
        (len > 0 && start.value().checked_add(len).is_some()).then_some(Self { start, len })
    }
    pub fn end(&self) -> u64 {
        self.start.value() + self.len
    }
}

/// Ordered, disjoint and nonadjacent address intervals.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AddressRanges(Vec<AddressRange>);

impl AddressRanges {
    pub fn single(start: CurrencyAddress, len: u64) -> Option<Self> {
        if len == 0 {
            return Some(Self::default());
        }
        Some(Self(vec![AddressRange::new(start, len)?]))
    }
    pub fn from_canonical(ranges: Vec<AddressRange>) -> Option<Self> {
        let mut end = None;
        for range in &ranges {
            AddressRange::new(range.start, range.len)?;
            if end.is_some_and(|end| end >= range.start.value()) {
                return None;
            }
            end = Some(range.end());
        }
        Some(Self(ranges))
    }
    pub fn ranges(&self) -> &[AddressRange] {
        &self.0
    }
    pub fn len(&self) -> u64 {
        self.0.iter().map(|range| range.len).sum()
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub fn insert(&mut self, range: AddressRange) {
        assert!(AddressRange::new(range.start, range.len).is_some());
        let left = self
            .0
            .partition_point(|part| part.end() < range.start.value());
        let mut right = left;
        let mut start = range.start.value();
        let mut end = range.end();
        while right < self.0.len() && self.0[right].start.value() <= end {
            start = start.min(self.0[right].start.value());
            end = end.max(self.0[right].end());
            right += 1;
        }
        self.0.splice(
            left..right,
            [AddressRange {
                start: CurrencyAddress::new(start),
                len: end - start,
            }],
        );
    }
    pub fn union_with(&mut self, other: &Self) {
        for range in &other.0 {
            self.insert(*range);
        }
    }
    pub fn difference(&self, other: &Self) -> Self {
        let mut result = Vec::new();
        let mut index = 0;
        for range in &self.0 {
            let mut cursor = range.start.value();
            while index < other.0.len() && other.0[index].end() <= cursor {
                index += 1;
            }
            let mut scan = index;
            while scan < other.0.len() && other.0[scan].start.value() < range.end() {
                let cut = other.0[scan];
                if cut.start.value() > cursor {
                    result.push(AddressRange {
                        start: CurrencyAddress::new(cursor),
                        len: cut.start.value() - cursor,
                    });
                }
                cursor = cursor.max(cut.end());
                if cursor >= range.end() {
                    break;
                }
                scan += 1;
            }
            if cursor < range.end() {
                result.push(AddressRange {
                    start: CurrencyAddress::new(cursor),
                    len: range.end() - cursor,
                });
            }
        }
        Self(result)
    }
    pub fn take(&self, mut count: u64) -> Self {
        let mut ranges = Vec::new();
        for range in &self.0 {
            if count == 0 {
                break;
            }
            let len = range.len.min(count);
            ranges.push(AddressRange { len, ..*range });
            count -= len;
        }
        Self(ranges)
    }
    pub fn intersects(&self, other: &Self) -> bool {
        let mut index = 0;
        for range in &self.0 {
            while index < other.0.len() && other.0[index].end() <= range.start.value() {
                index += 1;
            }
            if index < other.0.len() && other.0[index].start.value() < range.end() {
                return true;
            }
        }
        false
    }
    // Expansion is only for explicit signed lists and small test assertions.
    pub fn addresses(&self) -> impl Iterator<Item = CurrencyAddress> + '_ {
        self.0
            .iter()
            .flat_map(|range| (range.start.value()..range.end()).map(CurrencyAddress::new))
    }
}

impl FromIterator<CurrencyAddress> for AddressRanges {
    fn from_iter<T: IntoIterator<Item = CurrencyAddress>>(addresses: T) -> Self {
        let mut result = Self::default();
        for address in addresses {
            result.insert(AddressRange::new(address, 1).expect("allocatable address"));
        }
        result
    }
}

#[cfg(test)]
mod tests;
