#[derive(PartialEq, Eq, Clone, Hash)]
pub(crate) enum KnownVariable {
    This,
    Args,
    Context,
    Dollar,
    AtSign,
}

impl KnownVariable {
    pub(crate) fn from_str(var_name: &str) -> Option<Self> {
        match var_name {
            "$this" => Some(Self::This),
            "$args" => Some(Self::Args),
            "$context" => Some(Self::Context),
            "$" => Some(Self::Dollar),
            "@" => Some(Self::AtSign),
            _ => None,
        }
    }

    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::This => "$this",
            Self::Args => "$args",
            Self::Context => "$context",
            Self::Dollar => "$",
            Self::AtSign => "@",
        }
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
