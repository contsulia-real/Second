use super::*;
use std::collections::BTreeSet;

#[test]
fn range_subtraction_selection_and_normalization_match_address_sets() {
    let mut random = 91_u64;
    for _ in 0..200 {
        let mut sets = [AddressRanges::default(), AddressRanges::default()];
        let mut reference = [BTreeSet::new(), BTreeSet::new()];
        for index in 0..80 {
            random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
            let start = (random >> 32) % 200;
            let len = (random >> 16) % 20 + 1;
            sets[index % 2].insert(AddressRange::new(CurrencyAddress::new(start), len).unwrap());
            reference[index % 2].extend((start..start + len).map(CurrencyAddress::new));
        }
        let remaining = sets[0].difference(&sets[1]);
        assert_eq!(
            remaining.addresses().collect::<BTreeSet<_>>(),
            reference[0].difference(&reference[1]).copied().collect()
        );
        assert_eq!(
            sets[0].intersects(&sets[1]),
            !reference[0].is_disjoint(&reference[1])
        );
        assert_eq!(
            remaining.take(17).addresses().collect::<Vec<_>>(),
            remaining.addresses().take(17).collect::<Vec<_>>()
        );
        assert!(AddressRanges::from_canonical(remaining.ranges().to_vec()).is_some());
    }
    assert!(AddressRange::new(CurrencyAddress::new(u64::MAX), 1).is_none());
    assert!(
        AddressRanges::from_canonical(vec![
            AddressRange {
                start: CurrencyAddress::new(0),
                len: 1
            },
            AddressRange {
                start: CurrencyAddress::new(1),
                len: 1
            }
        ])
        .is_none()
    );
}
