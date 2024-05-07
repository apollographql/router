// The JSONSelection syntax is intended to be more generic than GraphQL, capable
// of transforming any aribitrary JSON in arbitrary ways, without assuming the
// universal availability of __typename or other convenient GraphQL-isms.
// However, since we are using the JSONSelection syntax to generate
// GraphQL-shaped output JSON, it's helpful to have some GraphQL-specific
// utilities.
//
// This file contains several trait implementations that allow converting from
// the JSONSelection type to a corresponding GraphQL selection set, where (for
// example) PathSelection syntax is expanded to ordinary nested selection sets.
// The resulting JSON will retain the nested structure of the GraphQL selection
// sets, and thus be more verbose than the output of the JSONSelection syntax,
// but may be easier to use for validating the selection against a GraphQL
// schema, using existing code for validating GraphQL operations.

use apollo_compiler::ast;
use apollo_compiler::ast::Selection as GraphQLSelection;

use super::parser::JSONSelection;
use super::parser::NamedSelection;
use super::parser::PathSelection;
use super::parser::StarSelection;
use super::parser::SubSelection;

#[derive(Default)]
struct GraphQLSelections(Vec<Result<GraphQLSelection, String>>);

impl GraphQLSelections {
    fn valid_selections(self) -> Vec<GraphQLSelection> {
        self.0.into_iter().filter_map(|i| i.ok()).collect()
    }
}

impl From<Vec<GraphQLSelection>> for GraphQLSelections {
    fn from(val: Vec<GraphQLSelection>) -> Self {
        Self(val.into_iter().map(Ok).collect())
    }
}

impl From<JSONSelection> for Vec<GraphQLSelection> {
    fn from(val: JSONSelection) -> Vec<GraphQLSelection> {
        match val {
            JSONSelection::Named(named_selections) => {
                GraphQLSelections::from(named_selections).valid_selections()
            }
            JSONSelection::Path(path_selection) => path_selection.into(),
        }
    }
}

fn new_field(name: String, selection: Option<GraphQLSelections>) -> GraphQLSelection {
    GraphQLSelection::Field(
        apollo_compiler::ast::Field {
            alias: None,
            name: ast::Name::new_unchecked(name.into()),
            arguments: Default::default(),
            directives: Default::default(),
            selection_set: selection
                .map(GraphQLSelections::valid_selections)
                .unwrap_or_default(),
        }
        .into(),
    )
}

impl From<NamedSelection> for Vec<GraphQLSelection> {
    fn from(val: NamedSelection) -> Vec<GraphQLSelection> {
        match val {
            NamedSelection::Field(alias, name, selection) => vec![new_field(
                alias.map(|a| a.name).unwrap_or(name),
                selection.map(|s| s.into()),
            )],
            NamedSelection::Quoted(alias, _name, selection) => {
                vec![new_field(
                    alias.name,
                    selection.map(GraphQLSelections::from),
                )]
            }
            NamedSelection::Path(alias, path_selection) => {
                let graphql_selection: Vec<GraphQLSelection> = path_selection.into();
                vec![new_field(
                    alias.name,
                    Some(GraphQLSelections::from(graphql_selection)),
                )]
            }
            NamedSelection::Group(alias, sub_selection) => {
                vec![new_field(alias.name, Some(sub_selection.into()))]
            }
        }
    }
}

impl From<PathSelection> for Vec<GraphQLSelection> {
    fn from(val: PathSelection) -> Vec<GraphQLSelection> {
        match val {
            PathSelection::Key(_, tail) => {
                let tail = *tail;
                tail.into()
            }
            PathSelection::Selection(selection) => {
                GraphQLSelections::from(selection).valid_selections()
            }
            PathSelection::Empty => vec![],
        }
    }
}

impl From<SubSelection> for GraphQLSelections {
    // give as much as we can, yield errors for star selection without alias.
    fn from(val: SubSelection) -> GraphQLSelections {
        let mut selections = val
            .selections
            .into_iter()
            .flat_map(|named_selection| {
                let selections: Vec<GraphQLSelection> = named_selection.into();
                GraphQLSelections::from(selections).0
            })
            .collect::<Vec<Result<GraphQLSelection, String>>>();

        if let Some(StarSelection(alias, sub_selection)) = val.star {
            if let Some(alias) = alias {
                let star = new_field(
                    alias.name,
                    sub_selection.map(|s| GraphQLSelections::from(*s)),
                );
                selections.push(Ok(star));
            } else {
                selections.push(Err(
                    "star selection without alias cannot be converted to GraphQL".to_string(),
                ));
            }
        }
        GraphQLSelections(selections)
    }
}

#[cfg(test)]
mod tests {
    use apollo_compiler::ast::Selection as GraphQLSelection;

    use crate::selection;

    fn print_set(set: &[apollo_compiler::ast::Selection]) -> String {
        set.iter()
            .map(|s| s.serialize().to_string())
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn into_selection_set() {
        let selection = selection!("f");
        let set: Vec<GraphQLSelection> = selection.into();
        assert_eq!(print_set(&set), "f");

        let selection = selection!("f f2 f3");
        let set: Vec<GraphQLSelection> = selection.into();
        assert_eq!(print_set(&set), "f f2 f3");

        let selection = selection!("f { f2 f3 }");
        let set: Vec<GraphQLSelection> = selection.into();
        assert_eq!(print_set(&set), "f {\n  f2\n  f3\n}");

        let selection = selection!("a: f { b: f2 }");
        let set: Vec<GraphQLSelection> = selection.into();
        assert_eq!(print_set(&set), "a {\n  b\n}");

        let selection = selection!(".a { b c }");
        let set: Vec<GraphQLSelection> = selection.into();
        assert_eq!(print_set(&set), "b c");

        let selection = selection!(".a.b { c: .d e }");
        let set: Vec<GraphQLSelection> = selection.into();
        assert_eq!(print_set(&set), "c e");

        let selection = selection!("a: { b c }");
        let set: Vec<GraphQLSelection> = selection.into();
        assert_eq!(print_set(&set), "a {\n  b\n  c\n}");

        let selection = selection!("a: 'quoted'");
        let set: Vec<GraphQLSelection> = selection.into();
        assert_eq!(print_set(&set), "a");

        let selection = selection!("a b: *");
        let set: Vec<GraphQLSelection> = selection.into();
        assert_eq!(print_set(&set), "a b");

        let selection = selection!("a *");
        let set: Vec<GraphQLSelection> = selection.into();
        assert_eq!(print_set(&set), "a");
    }
}
