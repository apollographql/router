use std::str::FromStr;

#[cfg(test)]
use super::location::WithRange;
use crate::sources::connect::variable::Namespace;

#[derive(PartialEq, Eq, Clone, Hash)]
pub(crate) enum KnownVariable {
    Identifier(Namespace),
    Dollar,
    AtSign,
}

impl KnownVariable {
    pub(crate) fn from_str(var_name: &str) -> Option<Self> {
        match var_name {
            "$" => Some(Self::Dollar),
            "@" => Some(Self::AtSign),
            s => Namespace::from_str(s).ok().map(Self::Identifier),
        }
    }

    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Identifier(namespace) => namespace.as_str(),
            Self::Dollar => "$",
            Self::AtSign => "@",
        }
    }

    #[cfg(test)]
    pub(super) fn into_with_range(self) -> WithRange<Self> {
        WithRange::new(self, None)
    }
}

impl std::fmt::Debug for KnownVariable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl std::fmt::Display for KnownVariable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl From<Namespace> for KnownVariable {
    fn from(namespace: Namespace) -> Self {
        Self::Identifier(namespace)
    }
}
