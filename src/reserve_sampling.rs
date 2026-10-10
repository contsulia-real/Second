use sha2::{Digest, Sha256};

use crate::{AddressRanges, CurrencyAddress};

const SAMPLING_DOMAIN: &[u8] = b"SECOND_RESERVE_SAMPLING_V1\0";

pub(crate) struct ReserveSamplingSeed([u8; 32]);

impl ReserveSamplingSeed {
    pub(crate) fn new(request_digest: [u8; 32], operation_index: u64) -> Self {
        let mut hash = Sha256::new();
        hash.update(SAMPLING_DOMAIN);
        hash.update(request_digest);
        hash.update(operation_index.to_be_bytes());
        Self(hash.finalize().into())
    }

    pub(crate) fn select(&self, available: &AddressRanges, count: u64) -> AddressRanges {
        assert!(count <= available.len());
        // This index belongs only to this draw batch; the ledger remains authoritative.
        let mut total = 0;
        let cumulative = available
            .ranges()
            .iter()
            .map(|range| {
                let before = total;
                total += range.len;
                before
            })
            .collect::<Vec<u64>>();
        let prefix_count = |bound: u64| {
            let index = available
                .ranges()
                .partition_point(|range| range.start.value() < bound);
            if index == 0 {
                0
            } else {
                let range = available.ranges()[index - 1];
                cumulative[index - 1] + (bound - range.start.value()).min(range.len)
            }
        };
        let mut selected = Vec::<u64>::with_capacity(count as usize);
        for draw in 0..count {
            let mut prefix = 0;
            let mut remaining = total - draw;
            for depth in 0_u8..64 {
                let middle = prefix + (1_u64 << (63 - depth));
                let removed = selected.partition_point(|value| *value < middle)
                    - selected.partition_point(|value| *value < prefix);
                let left = prefix_count(middle) - prefix_count(prefix) - removed as u64;
                let right = remaining - left;
                let go_left = if left == 0 {
                    false
                } else if right == 0 {
                    true
                } else {
                    let mut hash = Sha256::new();
                    hash.update(self.0);
                    hash.update(draw.to_be_bytes());
                    hash.update([depth]);
                    hash.update(prefix.to_be_bytes());
                    let bytes = hash.finalize();
                    let random = u64::from_be_bytes(bytes[..8].try_into().unwrap());
                    (random as u128) * (remaining as u128) < (left as u128) << 64
                };
                if go_left {
                    remaining = left;
                } else {
                    prefix = middle;
                    remaining = right;
                }
            }
            assert_eq!(remaining, 1);
            let index = selected.partition_point(|value| *value < prefix);
            selected.insert(index, prefix);
        }
        selected.into_iter().map(CurrencyAddress::new).collect()
    }

    pub(crate) fn pair(&self, selection: &AddressRanges) -> Vec<CurrencyAddress> {
        let mut ranked = selection
            .addresses()
            .map(|address| {
                let mut hash = Sha256::new();
                hash.update(self.0);
                hash.update(b"pair");
                hash.update(address.value().to_be_bytes());
                let digest: [u8; 32] = hash.finalize().into();
                (digest, address)
            })
            .collect::<Vec<_>>();
        ranked.sort_unstable();
        ranked.into_iter().map(|(_, address)| address).collect()
    }
}

#[cfg(test)]
mod tests;
