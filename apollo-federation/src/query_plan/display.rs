use std::fmt;

use apollo_compiler::executable;

use super::*;
use crate::display_helpers::write_indented_lines;
use crate::display_helpers::State;

impl QueryPlan {
    fn write_indented(&self, state: &mut State<'_, '_>) -> fmt::Result {
        let Self {
            node,
            statistics: _,
        } = self;
        state.write("QueryPlan {")?;
        if let Some(node) = node {
            state.indent()?;
            node.write_indented(state)?;
            state.dedent()?;
        }
        state.write("}")
    }
}

impl TopLevelPlanNode {
    fn write_indented(&self, state: &mut State<'_, '_>) -> fmt::Result {
        match self {
            Self::Subscription(node) => node.write_indented(state),
            Self::Fetch(node) => node.write_indented(state),
            Self::Sequence(node) => node.write_indented(state),
            Self::Parallel(node) => node.write_indented(state),
            Self::Flatten(node) => node.write_indented(state),
            Self::Defer(node) => node.write_indented(state),
            Self::Condition(node) => node.write_indented(state),
        }
    }
}

impl PlanNode {
    fn write_indented(&self, state: &mut State<'_, '_>) -> fmt::Result {
        match self {
            Self::Fetch(node) => node.write_indented(state),
            Self::Sequence(node) => node.write_indented(state),
            Self::Parallel(node) => node.write_indented(state),
            Self::Flatten(node) => node.write_indented(state),
            Self::Defer(node) => node.write_indented(state),
            Self::Condition(node) => node.write_indented(state),
        }
    }
}

impl SubscriptionNode {
    fn write_indented(&self, state: &mut State<'_, '_>) -> fmt::Result {
        let Self { primary, rest } = self;
        state.write("Subscription {")?;
        state.indent()?;

        state.write("Primary: {")?;
        state.indent()?;
        primary.write_indented(state)?;
        state.dedent()?;
        state.write("},")?;

        if let Some(rest) = rest {
            state.new_line()?;
            state.write("Rest: {")?;
            state.indent()?;
            rest.write_indented(state)?;
            state.dedent()?;
            state.write("},")?;
        }

        state.dedent()?;
        state.write("},")
    }
}

impl FetchNode {
    fn write_indented(&self, state: &mut State<'_, '_>) -> fmt::Result {
        let Self {
            subgraph_name,
            id,
            variable_usages: _,
            requires,
            operation_document,
            operation_name: _,
            operation_kind: _,
            input_rewrites: _,
            output_rewrites: _,
            context_rewrites: _,
        } = self;
        state.write(format_args!("Fetch(service: {subgraph_name:?}"))?;
        if let Some(id) = id {
            state.write(format_args!(", id: {id:?}"))?;
        }
        state.write(") {")?;
        state.indent()?;

        if let Some(v) = requires.as_ref() {
            if !v.is_empty() {
                write_selections(state, v)?;
                state.write(" =>")?;
                state.new_line()?;
            }
        }
        write_operation(state, operation_document)?;

        state.dedent()?;
        state.write("},")
    }
}

impl SequenceNode {
    fn write_indented(&self, state: &mut State<'_, '_>) -> fmt::Result {
        let Self { nodes } = self;
        state.write("Sequence {")?;

        write_indented_lines(state, nodes, |state, node| node.write_indented(state))?;

        state.write("},")
    }
}

impl ParallelNode {
    fn write_indented(&self, state: &mut State<'_, '_>) -> fmt::Result {
        let Self { nodes } = self;
        state.write("Parallel {")?;

        write_indented_lines(state, nodes, |state, node| node.write_indented(state))?;

        state.write("},")
    }
}

impl FlattenNode {
    fn write_indented(&self, state: &mut State<'_, '_>) -> fmt::Result {
        let Self { path, node } = self;
        state.write("Flatten(path: \"")?;
        if let Some((first, rest)) = path.split_first() {
            state.write(first)?;
            for element in rest {
                state.write(".")?;
                state.write(element)?;
            }
        }
        state.write("\") {")?;
        state.indent()?;

        node.write_indented(state)?;

        state.dedent()?;
        state.write("},")
    }
}

impl ConditionNode {
    fn write_indented(&self, state: &mut State<'_, '_>) -> fmt::Result {
        let Self {
            condition_variable,
            if_clause,
            else_clause,
        } = self;
        match (if_clause, else_clause) {
            (Some(if_clause), Some(else_clause)) => {
                state.write(format_args!("Condition(if: ${condition_variable}) {{"))?;
                state.indent()?;

                state.write("Then {")?;
                state.indent()?;
                if_clause.write_indented(state)?;
                state.dedent()?;
                state.write("}")?;

                state.write(" Else {")?;
                state.indent()?;
                else_clause.write_indented(state)?;
                state.dedent()?;
                state.write("},")?;

                state.dedent()?;
                state.write("},")
            }

            (Some(if_clause), None) => {
                state.write(format_args!("Include(if: ${condition_variable}) {{"))?;
                state.indent()?;

                if_clause.write_indented(state)?;

                state.dedent()?;
                state.write("},")
            }

            (None, Some(else_clause)) => {
                state.write(format_args!("Skip(if: ${condition_variable}) {{"))?;
                state.indent()?;

                else_clause.write_indented(state)?;

                state.dedent()?;
                state.write("},")
            }

            // Shouldn’t happen?
            (None, None) => state.write("Condition {}"),
        }
    }
}

impl DeferNode {
    fn write_indented(&self, state: &mut State<'_, '_>) -> fmt::Result {
        let Self { primary, deferred } = self;
        state.write("Defer {")?;
        state.indent()?;

        primary.write_indented(state)?;
        if !deferred.is_empty() {
            state.write(" [")?;
            write_indented_lines(state, deferred, |state, deferred| {
                deferred.write_indented(state)
            })?;
            state.write("]")?;
        }

        state.dedent()?;
        state.write("},")
    }
}

impl PrimaryDeferBlock {
    fn write_indented(&self, state: &mut State<'_, '_>) -> fmt::Result {
        let Self {
            sub_selection,
            node,
        } = self;
        state.write("Primary {")?;
        if sub_selection.is_some() || node.is_some() {
            if let Some(sub_selection) = sub_selection {
                // Manually indent and write the newline
                // to prevent a duplicate indent from `.new_line()` and `.initial_indent_level()`.
                state.indent_no_new_line();
                state.write("\n")?;

                state.write(
                    sub_selection
                        .serialize()
                        .initial_indent_level(state.indent_level()),
                )?;
                if node.is_some() {
                    state.write(":")?;
                    state.new_line()?;
                }
            } else {
                // Indent to match the Some() case
                state.indent()?;
            }

            if let Some(node) = node {
                node.write_indented(state)?;
            }

            state.dedent()?;
        }
        state.write("},")
    }
}

impl DeferredDeferBlock {
    fn write_indented(&self, state: &mut State<'_, '_>) -> fmt::Result {
        let Self {
            depends,
            label,
            query_path,
            sub_selection,
            node,
        } = self;

        state.write("Deferred(depends: [")?;
        if let Some((DeferredDependency { id }, rest)) = depends.split_first() {
            state.write(id)?;
            for DeferredDependency { id } in rest {
                state.write(", ")?;
                state.write(id)?;
            }
        }
        state.write("], path: \"")?;
        if let Some((first, rest)) = query_path.split_first() {
            state.write(first)?;
            for element in rest {
                state.write("/")?;
                state.write(element)?;
            }
        }
        state.write("\"")?;
        if let Some(label) = label {
            state.write_fmt(format_args!(r#", label: "{label}""#))?;
        }
        state.write(") {")?;

        if sub_selection.is_some() || node.is_some() {
            state.indent()?;

            if let Some(sub_selection) = sub_selection {
                write_selections(state, &sub_selection.selections)?;
                state.write(":")?;
                state.new_line()?;
            }
            if let Some(node) = node {
                node.write_indented(state)?;
            }

            state.dedent()?;
        }

        state.write("},")
    }
}

/// When we serialize a query plan, we want to serialize the operation
/// but not show the root level `query` definition or the `_entities` call.
/// This function flattens those nodes to only show their selection sets
fn write_operation(
    state: &mut State<'_, '_>,
    operation_document: &ExecutableDocument,
) -> fmt::Result {
    let operation = operation_document
        .operations
        .get(None)
        .expect("expected a single-operation document");
    write_selections(state, &operation.selection_set.selections)?;
    for fragment in operation_document.fragments.values() {
        state.write("\n\n")?; // new line without indentation (since `fragment` adds indentation)
        state.write(
            fragment
                .serialize()
                .initial_indent_level(state.indent_level()),
        )?
    }
    Ok(())
}

fn write_selections(
    state: &mut State<'_, '_>,
    mut selections: &[executable::Selection],
) -> fmt::Result {
    if let Some(executable::Selection::Field(field)) = selections.first() {
        if field.name == "_entities" {
            selections = &field.selection_set.selections
        }
    }
    state.write("{")?;

    // Manually indent and write the newline
    // to prevent a duplicate indent from `.new_line()` and `.initial_indent_level()`.
    state.indent_no_new_line();
    for sel in selections {
        state.write("\n")?;
        state.write(sel.serialize().initial_indent_level(state.indent_level()))?;
    }
    state.dedent()?;

    state.write("}")
}

/// PORT_NOTE: Corresponds to `GroupPath.updatedResponsePath` in `buildPlan.ts`
impl fmt::Display for FetchDataPathElement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Key(name, conditions) => {
                f.write_str(name)?;
                write_conditions(conditions, f)
            }
            Self::AnyIndex(conditions) => {
                f.write_str("@")?;
                write_conditions(conditions, f)
            }
            Self::TypenameEquals(name) => write!(f, "... on {name}"),
        }
    }
}

fn write_conditions(conditions: &[Name], f: &mut fmt::Formatter<'_>) -> fmt::Result {
    if !conditions.is_empty() {
        write!(f, "|[{}]", conditions.join(","))
    } else {
        Ok(())
    }
}

impl fmt::Display for QueryPathElement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Field(field) => f.write_str(field.response_key()),
            Self::InlineFragment(inline) => {
                if let Some(cond) = &inline.type_condition {
                    write!(f, "... on {cond}")
                } else {
                    Ok(())
                }
            }
        }
    }
}

macro_rules! impl_display {
    ($( $Ty: ty )+) => {
        $(
            impl fmt::Display for $Ty {
                fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
                    self.write_indented(&mut State::new(output))
                }
            }
        )+
    };
}

impl_display! {
    QueryPlan
    TopLevelPlanNode
    PlanNode
    SubscriptionNode
    FetchNode
    SequenceNode
    ParallelNode
    FlattenNode
    ConditionNode
    DeferNode
    PrimaryDeferBlock
    DeferredDeferBlock
}
