//! Variables used in connector directives `@connect` and `@source`.

use std::borrow::Cow;
use std::fmt::Display;
use std::fmt::Formatter;
use std::ops::Range;
use std::str::FromStr;

use apollo_compiler::ast::FieldDefinition;
use apollo_compiler::schema::Component;
use apollo_compiler::schema::ObjectType;
use apollo_compiler::Node;
use itertools::Itertools;

/// The context of an expression containing variable references. The context determines what
/// variable namespaces are available for use in the expression.
pub(crate) trait ExpressionContext<N: FromStr + ToString> {
    /// Get the variable namespaces that are available in this context
    fn available_namespaces(&self) -> impl Iterator<Item = N>;

    /// Get the list of namespaces joined as a comma separated list
    fn namespaces_joined(&self) -> String {
        self.available_namespaces()
            .map(|s| s.to_string())
            .sorted()
            .join(", ")
    }
}

/// A variable context for Apollo Connectors. Variables are used within a `@connect` or `@source`
/// [`Directive`], are used in a particular [`Phase`], and have a specific [`Target`].
#[derive(Clone, PartialEq)]
pub(crate) struct ConnectorsContext<'schema> {
    pub(super) directive: Directive<'schema>,
    pub(super) phase: Phase,
    pub(super) target: Target,
}

impl<'schema> ConnectorsContext<'schema> {
    pub(crate) fn new(directive: Directive<'schema>, phase: Phase, target: Target) -> Self {
        Self {
            directive,
            phase,
            target,
        }
    }
}

impl<'schema> ExpressionContext<Namespace> for ConnectorsContext<'schema> {
    fn available_namespaces(&self) -> impl Iterator<Item = Namespace> {
        match (&self.directive, &self.phase, &self.target) {
            (Directive::Source, Phase::Response, _) => {
                vec![Namespace::Config, Namespace::Context, Namespace::Status]
            }
            (Directive::Source, _, _) => vec![Namespace::Config, Namespace::Context],
            (Directive::Connect { .. }, Phase::Response, _) => {
                vec![
                    Namespace::Args,
                    Namespace::Config,
                    Namespace::Context,
                    Namespace::Status,
                    Namespace::This,
                ]
            }
            (Directive::Connect { .. }, _, _) => {
                vec![
                    Namespace::Config,
                    Namespace::Context,
                    Namespace::This,
                    Namespace::Args,
                ]
            }
        }
        .into_iter()
    }
}

/// An Apollo Connectors directive in a schema
#[derive(Clone, PartialEq)]
pub(crate) enum Directive<'schema> {
    /// A `@source` directive
    Source,

    /// A `@connect` directive
    Connect {
        /// The object type containing the field the directive is on
        object: &'schema Node<ObjectType>,

        /// The field definition of the field the directive is on
        field: &'schema Component<FieldDefinition>,
    },
}

/// The phase an expression is associated with
#[allow(dead_code)]
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Phase {
    /// The request phase
    Request,

    /// The response phase
    Response,
}

/// The target of an expression containing a variable reference
#[allow(unused)]
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Target {
    /// The expression is used in an HTTP header
    Header,

    /// The expression is used in a URL
    Url,

    /// The expression is used in the body of a request or response
    Body,
}

/// The variable namespaces defined for Apollo Connectors
#[derive(PartialEq, Eq, Clone, Copy, Hash)]
pub enum Namespace {
    Args,
    Config,
    Context,
    Status,
    This,
}

impl Namespace {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Args => "$args",
            Self::Config => "$config",
            Self::Context => "$context",
            Self::Status => "$status",
            Self::This => "$this",
        }
    }
}

impl FromStr for Namespace {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "$args" => Ok(Self::Args),
            "$config" => Ok(Self::Config),
            "$context" => Ok(Self::Context),
            "$status" => Ok(Self::Status),
            "$this" => Ok(Self::This),
            _ => Err(()),
        }
    }
}

impl std::fmt::Debug for Namespace {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl Display for Namespace {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// A variable reference. Consists of a namespace starting with a `$` and an optional path
/// separated by '.' characters.
#[derive(Debug, Eq, PartialEq, Clone, Hash)]
pub(crate) struct VariableReference<'a, N: FromStr + ToString> {
    /// The namespace of the variable - `$this`, `$args`, `$status`, etc.
    pub(crate) namespace: VariableNamespace<N>,

    /// The path elements of this reference. For example, the reference `$this.a.b.c`
    /// has path elements `a`, `b`, `c`. May be empty in some cases, as in the reference `$status`.
    pub(crate) path: Vec<VariablePathPart<'a>>,

    /// The location of the reference within the original text.
    pub(crate) location: Range<usize>,
}

impl<N: FromStr + ToString> Display for VariableReference<'_, N> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.namespace.namespace.to_string().as_str())?;
        for part in &self.path {
            f.write_str(".")?;
            f.write_str(part.as_str())?;
        }
        Ok(())
    }
}

/// A namespace in a variable reference, like `$this` in `$this.a.b.c`
#[derive(Debug, Eq, PartialEq, Clone, Hash)]
pub(crate) struct VariableNamespace<N: FromStr + ToString> {
    pub(crate) namespace: N,
    pub(crate) location: Range<usize>,
}

/// Part of a variable path, like `a` in `$this.a.b.c`
#[derive(Debug, Eq, PartialEq, Clone, Hash)]
pub(crate) struct VariablePathPart<'a> {
    pub(crate) part: Cow<'a, str>,
    pub(crate) location: Range<usize>,
}

impl VariablePathPart<'_> {
    pub(crate) fn as_str(&self) -> &str {
        self.part.as_ref()
    }
}

impl Display for VariablePathPart<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.part.to_string().as_str())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_available_namespaces() {
        assert_eq!(
            ConnectorsContext::new(Directive::Source, Phase::Request, Target::Url)
                .available_namespaces()
                .collect::<Vec<_>>(),
            vec![Namespace::Config, Namespace::Context,]
        );
        assert_eq!(
            ConnectorsContext::new(Directive::Source, Phase::Request, Target::Header)
                .available_namespaces()
                .collect::<Vec<_>>(),
            vec![Namespace::Config, Namespace::Context,]
        );
        assert_eq!(
            ConnectorsContext::new(Directive::Source, Phase::Request, Target::Body)
                .available_namespaces()
                .collect::<Vec<_>>(),
            vec![Namespace::Config, Namespace::Context,]
        );
        assert_eq!(
            ConnectorsContext::new(Directive::Source, Phase::Response, Target::Header)
                .available_namespaces()
                .collect::<Vec<_>>(),
            vec![Namespace::Config, Namespace::Context, Namespace::Status,]
        );
    }
}
