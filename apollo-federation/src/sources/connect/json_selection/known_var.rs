use std::borrow::Cow;

#[cfg(test)]
use super::location::WithRange;
use crate::sources::connect::variable::Namespace;
use crate::sources::connect::variable::VariableNamespace;
use crate::sources::connect::variable::VariablePathPart;
use crate::sources::connect::variable::VariableReference;

#[derive(PartialEq, Eq, Clone, Hash)]
pub(crate) enum KnownVariable {
    This,
    Args,
    Config,
    Status,
    Dollar,
    AtSign,
}

impl KnownVariable {
    pub(crate) fn from_str(var_name: &str) -> Option<Self> {
        match var_name {
            "$this" => Some(Self::This),
            "$args" => Some(Self::Args),
            "$config" => Some(Self::Config),
            "$status" => Some(Self::Status),
            "$" => Some(Self::Dollar),
            "@" => Some(Self::AtSign),
            _ => None,
        }
    }

    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::This => "$this",
            Self::Args => "$args",
            Self::Config => "$config",
            Self::Status => "$status",
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

impl TryFrom<(&KnownVariable, Vec<&str>)> for VariableReference<'static, Namespace> {
    type Error = ();

    fn try_from((variable, path): (&KnownVariable, Vec<&str>)) -> Result<Self, Self::Error> {
        let namespace = match variable {
            KnownVariable::Args => Namespace::Args,
            KnownVariable::This => Namespace::This,
            KnownVariable::Config => Namespace::Config,
            KnownVariable::Status => Namespace::Status,
            // To get the safety benefits of the KnownVariable enum, we need
            // to enumerate all the cases explicitly, without wildcard
            // matches. However, body.external_var_paths() only returns free
            // (externally-provided) variables like $this, $args, and
            // $config. The $ and @ variables, by contrast, are always bound
            // to something within the input data.
            KnownVariable::Dollar | KnownVariable::AtSign => return Err(()),
        };

        Ok(VariableReference {
            namespace: VariableNamespace {
                namespace,
                location: Default::default(),
            },
            path: path
                .iter()
                .map(|&key| VariablePathPart {
                    part: Cow::from(key.to_owned()),
                    location: Default::default(),
                })
                .collect(),
            location: Default::default(), // Doesn't matter for this case, we won't report errors here
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_try_from_variable_reference() {
        let variable = KnownVariable::This;
        let path = vec!["foo", "bar"];
        let result = VariableReference::<Namespace>::try_from((&variable, path));
        assert!(result.is_ok());
        assert_eq!(
            result.unwrap(),
            VariableReference {
                namespace: VariableNamespace {
                    namespace: Namespace::This,
                    location: Default::default(),
                },
                path: vec![
                    VariablePathPart {
                        part: Cow::from("foo"),
                        location: Default::default(),
                    },
                    VariablePathPart {
                        part: Cow::from("bar"),
                        location: Default::default(),
                    },
                ],
                location: Default::default(),
            }
        );
        assert!(
            VariableReference::<Namespace>::try_from((&KnownVariable::Dollar, vec![])).is_err()
        );
    }
}
