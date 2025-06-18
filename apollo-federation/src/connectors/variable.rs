//! Variables used in connector directives `@connect` and `@source`.

use std::fmt::Display;
use std::fmt::Formatter;
use std::ops::Range;
use std::str::FromStr;

use itertools::Itertools;

use super::id::ConnectedElement;
use super::json_selection::SelectionTrie;
use crate::connectors::validation::Code;

/// A variable context for Apollo Connectors. Variables are used within a `@connect` or `@source`
/// [`Directive`], are used in a particular [`Phase`], and have a specific [`Target`].
#[derive(Clone, PartialEq)]
pub(crate) struct VariableContext<'schema> {
    /// The field definition or type the directive is on
    pub(crate) element: &'schema ConnectedElement<'schema>,

    pub(super) phase: Phase,
    pub(super) target: Target,
}

impl<'schema> VariableContext<'schema> {
    pub(crate) const fn new(
        element: &'schema ConnectedElement<'schema>,
        phase: Phase,
        target: Target,
    ) -> Self {
        Self {
            element,
            phase,
            target,
        }
    }

    /// Get the variable namespaces that are available in this context
    pub(crate) fn available_namespaces(&self) -> impl Iterator<Item = Namespace> {
        match &self.phase {
            Phase::Response => {
                vec![
                    Namespace::Args,
                    Namespace::Config,
                    Namespace::Context,
                    Namespace::Status,
                    Namespace::This,
                    Namespace::Request,
                    Namespace::Response,
                    Namespace::Env,
                ]
            }
        }
        .into_iter()
    }

    /// Get the list of namespaces joined as a comma separated list
    pub(crate) fn namespaces_joined(&self) -> String {
        self.available_namespaces()
            .map(|s| s.to_string())
            .sorted()
            .join(", ")
    }

    /// Get the error code for this context
    pub(crate) const fn error_code(&self) -> Code {
        match self.target {
            Target::Body => Code::InvalidSelection,
        }
    }
}

/// The phase an expression is associated with
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Phase {
    /// The response phase
    Response,
}

/// The target of an expression containing a variable reference
#[allow(unused)]
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Target {
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
    Batch,
    Request,
    Response,
    Env,
}

impl Namespace {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Args => "$args",
            Self::Config => "$config",
            Self::Context => "$context",
            Self::Status => "$status",
            Self::This => "$this",
            Self::Batch => "$batch",
            Self::Request => "$request",
            Self::Response => "$response",
            Self::Env => "$env",
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
            "$batch" => Ok(Self::Batch),
            "$request" => Ok(Self::Request),
            "$response" => Ok(Self::Response),
            "$env" => Ok(Self::Env),
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
pub struct VariableReference<N: FromStr + ToString> {
    /// The namespace of the variable - `$this`, `$args`, `$status`, etc.
    pub namespace: VariableNamespace<N>,

    /// The path elements of this reference. For example, the reference `$this.a.b.c`
    /// has path elements `a`, `b`, `c`. May be empty in some cases, as in the reference `$status`.
    pub(crate) selection: SelectionTrie,

    /// The location of the reference within the original text.
    pub(crate) location: Option<Range<usize>>,
}

impl<N: FromStr + ToString> Display for VariableReference<N> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.namespace.namespace.to_string().as_str())?;
        if !self.selection.is_empty() {
            write!(f, " {{ {} }}", self.selection)?;
        }
        Ok(())
    }
}

/// A namespace in a variable reference, like `$this` in `$this.a.b.c`
#[derive(Debug, Eq, PartialEq, Clone, Hash)]
pub struct VariableNamespace<N: FromStr + ToString> {
    pub namespace: N,
    pub(crate) location: Option<Range<usize>>,
}
