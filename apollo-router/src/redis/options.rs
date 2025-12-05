use std::time::Duration;

pub(crate) mod expire {
    pub(crate) enum Options {
        // NX -- Set expiry only when the key has no expiry
        MissingOnly,
        // XX -- Set expiry only when the key has an existing expiry
        ExistingOnly,
        // GT -- Set expiry only when the new expiry is greater than current one
        GreaterThan,
        // LT -- Set expiry only when the new expiry is less than current one
        LessThan,
    }

    impl From<Options> for fred::types::ExpireOptions {
        fn from(value: Options) -> Self {
            match value {
                Options::MissingOnly => Self::NX,
                Options::ExistingOnly => Self::XX,
                Options::GreaterThan => Self::GT,
                Options::LessThan => Self::LT,
            }
        }
    }
}

#[derive(Clone)]
pub(crate) enum Expiration {
    // EX
    After(Duration),
    // EXAT
    At(i64),
}

impl From<Expiration> for fred::types::Expiration {
    fn from(value: Expiration) -> Self {
        match value {
            Expiration::After(duration) => Self::EX(duration.as_secs() as i64),
            Expiration::At(epoch) => Self::EXAT(epoch),
        }
    }
}

pub(crate) mod zadd {
    pub(crate) enum Ordering {
        GreaterThan,
    }

    impl From<Ordering> for fred::types::sorted_sets::Ordering {
        fn from(value: Ordering) -> Self {
            match value {
                Ordering::GreaterThan => Self::GreaterThan,
            }
        }
    }
}
