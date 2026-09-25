//! The `FieldSelectionMap` scalar of the GraphQL Federation (composite schemas) specification,
//! Appendix A.
//!
//! A `FieldSelectionMap` describes how an argument value is derived from fields of an output
//! type. It is used by `@is` (lookup arguments mapped from the lookup's return type) and
//! `@require` (arguments whose value the executor derives from the parent type). It is not a
//! GraphQL selection set, so `apollo-compiler` cannot parse it; this module implements the
//! grammar:
//!
//! ```text
//! SelectedValue      ::= `|`? SelectedValueEntry (`|` SelectedValueEntry)*
//! SelectedValueEntry ::= Path [lookahead != `.`]
//!                      | Path `.` SelectedObjectValue
//!                      | Path SelectedListValue
//!                      | SelectedObjectValue
//! Path               ::= (`<` TypeName `>` `.`)? PathSegment
//! PathSegment        ::= FieldName Arguments[Const]? (`<` TypeName `>`)? (`.` PathSegment)?
//! SelectedObjectValue::= `{` SelectedObjectField+ `}`
//! SelectedObjectField::= Name `:` SelectedValue | Name Arguments[Const]?
//! SelectedListValue  ::= `[` SelectedValue `]` | `[` SelectedListValue `]`
//! ```
//!
//! A type condition written after a field (`mediaById<Book>.isbn`) narrows the type of that field
//! before the next segment; the grammar only allows it when another segment follows.

mod parser;
pub(crate) mod validate;
pub(crate) mod value;

use std::fmt;

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast;

pub(crate) use self::parser::parse;

/// A parsed `FieldSelectionMap`: one or more alternatives separated by `|`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct SelectedValue {
    pub(crate) alternatives: Vec<SelectedValueEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum SelectedValueEntry {
    /// A path selecting a single (leaf) value.
    Path(Path),
    /// A path into a composite field, followed by an object selected relative to that field.
    PathObject(Path, SelectedObjectValue),
    /// A path into a list field, followed by the selection applied to each element.
    PathList(Path, SelectedListValue),
    /// An object selected relative to the current type.
    Object(SelectedObjectValue),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct Path {
    /// `<Type>.` prefix narrowing the current type before the first segment.
    pub(crate) type_condition: Option<Name>,
    pub(crate) segments: Vec<PathSegment>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct PathSegment {
    pub(crate) field: Name,
    pub(crate) arguments: Vec<Node<ast::Argument>>,
    /// `<Type>` written after the field, narrowing the field's type before the next segment.
    pub(crate) type_condition: Option<Name>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct SelectedObjectValue {
    pub(crate) fields: Vec<SelectedObjectField>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum SelectedObjectField {
    /// `name: <selected value>`.
    Labeled(Name, SelectedValue),
    /// `name(args)?`: the input field and the output field share a name.
    Shorthand(Name, Vec<Node<ast::Argument>>),
}

impl SelectedObjectField {
    pub(crate) fn name(&self) -> &Name {
        match self {
            Self::Labeled(name, _) | Self::Shorthand(name, _) => name,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum SelectedListValue {
    Value(SelectedValue),
    List(Box<SelectedListValue>),
}

/// A `FieldSelectionMap` failed to parse.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message} (at offset {offset})")]
pub(crate) struct FieldSelectionMapParseError {
    pub(crate) message: String,
    pub(crate) offset: usize,
}

impl SelectedValue {
    /// A selected value made of a single entry.
    pub(crate) fn single(entry: SelectedValueEntry) -> Self {
        Self {
            alternatives: vec![entry],
        }
    }

    /// A single-segment path selecting `field`, i.e. the implicit `@is` mapping of a lookup
    /// argument whose name matches a field of the return type.
    pub(crate) fn field(field: Name) -> Self {
        Self::single(SelectedValueEntry::Path(Path {
            type_condition: None,
            segments: vec![PathSegment {
                field,
                arguments: Vec::new(),
                type_condition: None,
            }],
        }))
    }
}

fn write_arguments(f: &mut fmt::Formatter<'_>, arguments: &[Node<ast::Argument>]) -> fmt::Result {
    if arguments.is_empty() {
        return Ok(());
    }
    f.write_str("(")?;
    for (i, argument) in arguments.iter().enumerate() {
        if i > 0 {
            f.write_str(", ")?;
        }
        write!(
            f,
            "{}: {}",
            argument.name,
            argument.value.serialize().no_indent()
        )?;
    }
    f.write_str(")")
}

impl fmt::Display for SelectedValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, entry) in self.alternatives.iter().enumerate() {
            if i > 0 {
                f.write_str(" | ")?;
            }
            write!(f, "{entry}")?;
        }
        Ok(())
    }
}

impl fmt::Display for SelectedValueEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Path(path) => write!(f, "{path}"),
            Self::PathObject(path, object) => write!(f, "{path}.{object}"),
            Self::PathList(path, list) => write!(f, "{path}{list}"),
            Self::Object(object) => write!(f, "{object}"),
        }
    }
}

impl fmt::Display for Path {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(type_condition) = &self.type_condition {
            write!(f, "<{type_condition}>.")?;
        }
        for (i, segment) in self.segments.iter().enumerate() {
            if i > 0 {
                f.write_str(".")?;
            }
            f.write_str(&segment.field)?;
            write_arguments(f, &segment.arguments)?;
            if let Some(type_condition) = &segment.type_condition {
                write!(f, "<{type_condition}>")?;
            }
        }
        Ok(())
    }
}

impl fmt::Display for SelectedObjectValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("{ ")?;
        for (i, field) in self.fields.iter().enumerate() {
            if i > 0 {
                f.write_str(", ")?;
            }
            match field {
                SelectedObjectField::Labeled(name, value) => write!(f, "{name}: {value}")?,
                SelectedObjectField::Shorthand(name, arguments) => {
                    f.write_str(name)?;
                    write_arguments(f, arguments)?;
                }
            }
        }
        f.write_str(" }")
    }
}

impl fmt::Display for SelectedListValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Value(value) => write!(f, "[{value}]"),
            Self::List(list) => write!(f, "[{list}]"),
        }
    }
}

#[cfg(test)]
mod tests;
