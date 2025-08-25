use std::fmt::Display;
use std::str::FromStr;

/// A namespace used in tests to avoid dependencies on specific external namespaces
#[derive(Debug, PartialEq)]
pub(super) enum Namespace {
    Args,
    This,
}

impl FromStr for Namespace {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "$args" => Ok(Self::Args),
            "$this" => Ok(Self::This),
            _ => Err(format!("Unknown variable namespace: {s}")),
        }
    }
}

impl Display for Namespace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Args => write!(f, "$args"),
            Self::This => write!(f, "$this"),
        }
    }
}
