use super::*;

#[test]
fn public_sync_budget_counts_ranges_before_extending_the_buffer() {
    let range = PublicCurrencyState {
        start: CurrencyAddress::new(0),
        len: 1_000_000_000,
        occupied: true,
    };
    let mut states = Vec::new();
    append_synced_page(&mut states, vec![range.clone()], 1).unwrap();
    assert!(matches!(
        append_synced_page(&mut states, vec![range.clone()], 1),
        Err(NetworkError::PublicCurrencySyncTooLarge {
            announced: 2,
            maximum: 1
        })
    ));
    assert_eq!(states, vec![range]);
}
