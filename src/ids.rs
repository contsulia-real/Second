macro_rules! id_type {
    ($name:ident, $inner:ty) => {
        #[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
        pub struct $name($inner);

        impl $name {
            pub const fn new(value: $inner) -> Self {
                Self(value)
            }

            pub const fn value(self) -> $inner {
                self.0
            }
        }
    };
}

id_type!(AccountAddress, u64);
id_type!(CurrencyAddress, u64);
id_type!(PaymentAddress, u64);
id_type!(TaskId, u128);
id_type!(ValidatorId, u64);

impl From<u64> for CurrencyAddress {
    fn from(value: u64) -> Self {
        Self::new(value)
    }
}
