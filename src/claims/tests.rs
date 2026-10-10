use super::*;

#[test]
fn restored_signed_lists_reject_unallocatable_addresses_without_panicking_or_claiming() {
    let mut claims = CurrencyClaimBook::new();
    let id = OperationClaimId::new(TaskId::parse("boundary").unwrap(), 0);
    let invalid = [CurrencyAddress::new(u64::MAX)];
    assert!(matches!(
        claims.restore_leak_repair(id, &invalid, &AddressRanges::default()),
        Err(ClaimError::ClaimIdentityConflict(_))
    ));
    assert_eq!(claims.claimed_currency_count(), 0);
}
