use serde::Deserialize;
use serde::Serialize;

/// A serializable diagnostic similar to a GraphQL
/// [error](https://spec.graphql.org/October2021/#sec-Errors.Error-result-format).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FormattingDiagnostic {
    /// The error message.
    pub(crate) message: String,

    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[serde(default)]
    pub(crate) path: Vec<ResponseDataPathElement>,
}

/// Linked-list version of `Vec<PathElement>`, taking advantage of the call stack
pub(super) type LinkedPath<'a> = Option<&'a LinkedPathElement<'a>>;

#[derive(Debug)]
pub(super) struct LinkedPathElement<'a> {
    pub(super) element: ResponseDataPathElement,
    pub(super) next: LinkedPath<'a>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub(crate) enum ResponseDataPathElement {
    /// The relevant key in an object value
    Field(apollo_compiler::ast::Name),

    /// The index of the relevant item in a list value
    ListIndex(usize),
}

impl FormattingDiagnostic {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            path: Default::default(),
        }
    }

    pub(super) fn for_path(message: impl Into<String>, path: LinkedPath<'_>) -> Self {
        let mut err = Self::new(message);
        err.path = path_to_vec(path);
        err
    }
}

pub(super) fn path_to_vec(mut link: LinkedPath<'_>) -> Vec<ResponseDataPathElement> {
    let mut path = Vec::new();
    while let Some(node) = link {
        path.push(node.element.clone());
        link = node.next;
    }
    path.reverse();
    path
}
