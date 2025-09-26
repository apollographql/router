#[cfg(test)]
use super::location::WithRange;

#[derive(PartialEq, Eq, Clone, Hash)]
pub(crate) enum KnownVariable {
    External(String),
    Dollar,
    AtSign,
    /// The `input->as($var)` method binds `input` to the `$var` variable in the
    /// remainder of the path. Since references to such variables are always
    /// internal to the selection, neither referring to nor requiring external
    /// data, we use a separate enum variant and leave KnownVariable::External
    /// for variables that truly refer to external data.
    Local(String),
}

impl KnownVariable {
    pub(crate) fn as_str(&self) -> &str {
        match self {
            Self::External(namespace) => namespace.as_str(),
            Self::Dollar => "$",
            Self::AtSign => "@",
            Self::Local(namespace) => namespace.as_str(),
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
