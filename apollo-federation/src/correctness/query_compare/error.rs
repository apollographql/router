//! Structured explanation of why one query does not include another.
//!
//! The Lean model answers query inclusion with a `Bool`. That is enough to state soundness and
//! completeness, but it is not enough to act on a failure: `matchInclusionChildTask?` returns a
//! bare `none` whether the left side never selects the response name, selects it under a
//! different field, or passes different arguments. This module is the missing half — it keeps the
//! *position* of the failure and the *reason* for it apart, so a rejection can be read back as a
//! path through the query rather than re-derived by hand.
//!
//! [`ComparisonError::path`] is the checker's descent from the operation root, outermost segment
//! first; [`ComparisonError::reason`] is what failed at the end of it. Both render as indented
//! text ([`fmt::Display`]) or as JSON ([`ComparisonError::to_json`]).

use std::fmt;

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast;
use serde::Serialize;
use serde::Serializer;

use super::conditions::Assignment;
use crate::display_helpers::State;

fn serialize_display<T: fmt::Display, S: Serializer>(
    value: &T,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.collect_str(value)
}

/// Renders a list of arguments the way they appear in a query, for reporting.
pub(crate) fn render_arguments(arguments: &[Node<ast::Argument>]) -> String {
    if arguments.is_empty() {
        return "(no arguments)".to_string();
    }
    let rendered: Vec<String> = arguments
        .iter()
        .map(|argument| format!("{}: {}", argument.name, argument.value))
        .collect();
    format!("({})", rendered.join(", "))
}

//==================================================================================================
// Path

/// One step of the checker's descent.
///
/// The shape mirrors the recursion in `QueryInclusion.includesBool`: a response name is analyzed
/// over each type region, under each assignment of the Boolean variables that can affect it, and
/// then recursively through the merged sub-selection of its composite children.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PathSegment {
    /// Checking the field group that the right operation records under one response name.
    ResponseName { name: Name },

    /// Checking one region of runtime types. Regions partition the parent's possible types so
    /// that every type in a region is governed by exactly the same set of conditions.
    TypeRegion { types: Vec<Name> },

    /// Assuming one assignment of the `@skip`/`@include` variables that can affect this group.
    BooleanAssignment {
        #[serde(serialize_with = "serialize_display")]
        assignment: Assignment,
    },

    /// Recursing into the merged sub-selection of a composite field.
    ChildSelection {
        field: Name,
        /// The exact possible types of the field's return, not widened to its declared interface.
        possible_types: Vec<Name>,
    },
}

impl PathSegment {
    fn write_indented(&self, state: &mut State<'_, '_>) -> fmt::Result {
        match self {
            PathSegment::ResponseName { name } => {
                state.write(format_args!("in response name: {name}"))
            }
            PathSegment::TypeRegion { types } => {
                state.write(format_args!("over runtime types: {{{}}}", types.join(", ")))
            }
            PathSegment::BooleanAssignment { assignment } => {
                state.write(format_args!("assuming: {assignment}"))
            }
            PathSegment::ChildSelection {
                field,
                possible_types,
            } => state.write(format_args!(
                "in sub-selection of: {field} -> {{{}}}",
                possible_types.join(", ")
            )),
        }
    }
}

//==================================================================================================
// Reason

/// What failed at the end of a [`ComparisonError`]'s path.
///
/// The first group are genuine non-inclusion findings. The last group report inputs outside the
/// modeled fragment; those are *not* statements about inclusion.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum Mismatch {
    /// The two operations have different root types, so no response of one can include the other.
    RootTypeMismatch { left: Name, right: Name },

    /// A variable declared by both operations is declared differently. Shared declarations must
    /// agree before a static comparison says anything about runtime responses; declarations on
    /// only one side are unconstrained.
    VariableDeclarationMismatch {
        variable: Name,
        left: String,
        right: String,
    },

    /// The right operation selects this response name here, and the left operation does not
    /// select anything under it.
    MissingResponseName {
        response_name: Name,
        /// The field the right operation would have resolved.
        field_name: Name,
        runtime_type: Name,
    },

    /// Both operations select this response name, but resolve different fields for it. Response
    /// data can only be included if it comes from the same resolver call.
    FieldNameMismatch {
        response_name: Name,
        left: Name,
        right: Name,
    },

    /// Both operations resolve the same field for this response name, with different arguments.
    FieldArgumentsMismatch {
        response_name: Name,
        field_name: Name,
        left: String,
        right: String,
    },

    /// The right operation selects a field that the schema does not define on this parent type.
    /// Reachable only for operations that were not validated against this schema.
    UndefinedField { parent_type: Name, field_name: Name },

    /// A directive outside the modeled `@skip`/`@include` fragment. Not an inclusion finding.
    UnsupportedDirective { name: Name },

    /// A named fragment spread. Spreads must be inlined before comparison. Not an inclusion
    /// finding.
    UnsupportedFragmentSpread,

    /// The comparison could not be carried out. Not an inclusion finding.
    Internal { message: String },
}

impl Mismatch {
    /// Is this a statement about inclusion, as opposed to a rejected input or a defect?
    pub fn is_inclusion_finding(&self) -> bool {
        !matches!(
            self,
            Mismatch::UnsupportedDirective { .. }
                | Mismatch::UnsupportedFragmentSpread
                | Mismatch::Internal { .. }
        )
    }

    fn write_indented(&self, state: &mut State<'_, '_>) -> fmt::Result {
        match self {
            Mismatch::RootTypeMismatch { left, right } => state.write(format_args!(
                "root type differs: left is {left}, right is {right}"
            )),
            Mismatch::VariableDeclarationMismatch {
                variable,
                left,
                right,
            } => {
                state.write(format_args!("variable ${variable} is declared differently"))?;
                state.indent_no_new_line();
                state.new_line()?;
                state.write(format_args!("left:  {left}"))?;
                state.new_line()?;
                state.write(format_args!("right: {right}"))?;
                state.dedent_no_new_line();
                Ok(())
            }
            Mismatch::MissingResponseName {
                response_name,
                field_name,
                runtime_type,
            } => {
                if response_name == field_name {
                    state.write(format_args!(
                        "left does not select `{field_name}` on {runtime_type}"
                    ))
                } else {
                    state.write(format_args!(
                        "left does not select `{response_name}` on {runtime_type} \
                         (right resolves it with `{field_name}`)"
                    ))
                }
            }
            Mismatch::FieldNameMismatch {
                response_name,
                left,
                right,
            } => state.write(format_args!(
                "`{response_name}` resolves a different field: left selects `{left}`, \
                 right selects `{right}`"
            )),
            Mismatch::FieldArgumentsMismatch {
                response_name,
                field_name,
                left,
                right,
            } => {
                if response_name == field_name {
                    state.write(format_args!(
                        "`{field_name}` is called with different arguments"
                    ))?;
                } else {
                    state.write(format_args!(
                        "`{response_name}` resolves `{field_name}` with different arguments"
                    ))?;
                }
                state.indent_no_new_line();
                state.new_line()?;
                state.write(format_args!("left:  {left}"))?;
                state.new_line()?;
                state.write(format_args!("right: {right}"))?;
                state.dedent_no_new_line();
                Ok(())
            }
            Mismatch::UndefinedField {
                parent_type,
                field_name,
            } => state.write(format_args!(
                "{parent_type}.{field_name} is not defined in the schema"
            )),
            Mismatch::UnsupportedDirective { name } => state.write(format_args!(
                "@{name} is outside the modeled @skip/@include fragment"
            )),
            Mismatch::UnsupportedFragmentSpread => {
                state.write("named fragment spreads must be inlined before comparison")
            }
            Mismatch::Internal { message } => {
                state.write(format_args!("internal error: {message}"))
            }
        }
    }
}

//==================================================================================================
// ComparisonError

/// A structured explanation of why `includes` rejected a pair of operations.
#[derive(Debug, Clone, Serialize)]
pub struct ComparisonError {
    /// The descent to the failure, outermost segment first.
    path: Vec<PathSegment>,
    reason: Box<Mismatch>,
}

impl ComparisonError {
    pub fn new(reason: Mismatch) -> ComparisonError {
        ComparisonError {
            path: Vec::new(),
            reason: Box::new(reason),
        }
    }

    /// The comparison could not be carried out. Reserved for defects; it does not mean the two
    /// operations differ.
    pub fn internal(message: String) -> ComparisonError {
        ComparisonError::new(Mismatch::Internal { message })
    }

    /// Records that this failure was found one level inside `segment`. Called as the error
    /// unwinds, so the segment goes to the front of the path.
    pub fn add_context(mut self, segment: PathSegment) -> ComparisonError {
        self.path.insert(0, segment);
        self
    }

    pub fn path(&self) -> &[PathSegment] {
        &self.path
    }

    pub fn reason(&self) -> &Mismatch {
        &self.reason
    }

    /// Is this a statement about inclusion, as opposed to a rejected input or a defect?
    pub fn is_inclusion_finding(&self) -> bool {
        self.reason.is_inclusion_finding()
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_else(|e| {
            serde_json::Value::String(format!("failed to serialize ComparisonError: {e}"))
        })
    }
}

impl fmt::Display for ComparisonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = &mut State::new(f);
        state.write("left does not include right")?;
        state.indent_no_new_line();
        for segment in &self.path {
            state.new_line()?;
            segment.write_indented(state)?;
        }
        state.new_line()?;
        state.write("--> ")?;
        self.reason.write_indented(state)?;
        state.dedent_no_new_line();
        Ok(())
    }
}

/// Renders a variable declaration (type plus default) for a mismatch report.
pub(crate) fn display_variable_declaration(
    ty: &ast::Type,
    default_value: Option<&ast::Value>,
) -> String {
    match default_value {
        Some(value) => format!("{ty} = {value}"),
        None => ty.to_string(),
    }
}
