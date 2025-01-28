#[cfg(test)]
use super::location::WithRange;

#[derive(PartialEq, Eq, Clone, Hash)]
pub(crate) enum KnownVariable {
    External(String),
    Dollar,
    AtSign,
}

impl KnownVariable {
    pub(crate) fn from_str(var_name: &str) -> Self {
        match var_name {
            "$" => Self::Dollar,
            "@" => Self::AtSign,
            s => Self::External(s.to_string()),
        }
    }

    pub(crate) fn as_str(&self) -> &str {
        match self {
            Self::External(namespace) => namespace.as_str(),
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
