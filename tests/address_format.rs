use second::{
    AccountAddress, AddressParseError, CurrencyAddress, CurrencyAddressParseError, PaymentAddress,
};

const ZERO_PAYLOAD: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

#[test]
fn account_and_payment_addresses_use_distinct_canonical_base64url_forms() {
    let bytes = [0_u8; 32];
    let account = AccountAddress::from_bytes(bytes);
    let payment = PaymentAddress::from_bytes(bytes);

    let account_text = format!("acct_{ZERO_PAYLOAD}");
    let payment_text = format!("pay_{ZERO_PAYLOAD}");

    assert_eq!(account.to_string(), account_text);
    assert_eq!(payment.to_string(), payment_text);
    assert_eq!(AccountAddress::parse(&account_text), Ok(account));
    assert_eq!(PaymentAddress::parse(&payment_text), Ok(payment));

    assert_eq!(
        AccountAddress::parse(&payment_text),
        Err(AddressParseError::WrongPrefix)
    );
    assert_eq!(
        PaymentAddress::parse(&account_text),
        Err(AddressParseError::WrongPrefix)
    );
}

#[test]
fn addresses_reject_padding_and_wrong_payload_length() {
    let canonical = format!("acct_{ZERO_PAYLOAD}");

    assert_eq!(
        AccountAddress::parse(&(canonical.clone() + "=")),
        Err(AddressParseError::WrongEncodedLength)
    );
    assert_eq!(
        AccountAddress::parse(&canonical[..canonical.len() - 1]),
        Err(AddressParseError::WrongEncodedLength)
    );
}

#[test]
fn currency_address_uses_the_protocol_base62_alphabet_and_examples() {
    for (sequence, expected) in [
        (0, "0lxii0"),
        (9, "0lxii9"),
        (10, "0lxiia"),
        (11, "0lxiiA"),
        (61, "0lxiiZ"),
        (62, "0lxii10"),
    ] {
        let address = CurrencyAddress::new(sequence);
        assert_eq!(address.to_string(), expected);
        assert_eq!(CurrencyAddress::parse(expected), Ok(address));
    }
}

#[test]
fn currency_address_rejects_noncanonical_invalid_and_overflowing_forms() {
    assert_eq!(
        CurrencyAddress::parse("0lxii00"),
        Err(CurrencyAddressParseError::NonCanonical)
    );
    assert_eq!(
        CurrencyAddress::parse("0lxii!"),
        Err(CurrencyAddressParseError::InvalidCharacter)
    );
    assert_eq!(
        CurrencyAddress::parse("currency0"),
        Err(CurrencyAddressParseError::WrongPrefix)
    );
    assert_eq!(
        CurrencyAddress::parse("0lxiiZZZZZZZZZZZZ"),
        Err(CurrencyAddressParseError::Overflow)
    );

    let out_of_range = CurrencyAddress::new(second::MAX_CURRENCY_SEQUENCE + 1).to_string();
    assert_eq!(
        CurrencyAddress::parse(&out_of_range),
        Err(CurrencyAddressParseError::OutOfRange)
    );
}
