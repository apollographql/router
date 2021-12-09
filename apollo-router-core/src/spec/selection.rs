use apollo_parser::ast;

#[derive(Debug, Clone)]
pub(crate) enum Selection {
    Field {
        name: String,
        selection_set: Option<Vec<Selection>>,
    },
    InlineFragment {
        selection_set: Vec<Selection>,
    },
    FragmentSpread {
        name: String,
    },
}

impl From<ast::Selection> for Selection {
    fn from(selection: ast::Selection) -> Self {
        match selection {
            // Spec: https://spec.graphql.org/draft/#Field
            ast::Selection::Field(field) => {
                let name = field
                    .name()
                    .expect("the node Name is not optional in the spec; qed")
                    .text()
                    .to_string();
                let alias = field.alias().map(|x| x.name().unwrap().text().to_string());
                let name = alias.unwrap_or(name);
                let selection_set = field
                    .selection_set()
                    .map(|x| x.selections().into_iter().map(Into::into).collect());

                Self::Field {
                    name,
                    selection_set,
                }
            }
            // Spec: https://spec.graphql.org/draft/#InlineFragment
            ast::Selection::InlineFragment(inline_fragment) => {
                let selection_set = inline_fragment
                    .selection_set()
                    .expect("the node SelectionSet is not optional in the spec; qed")
                    .selections()
                    .into_iter()
                    .map(Into::into)
                    .collect();

                Self::InlineFragment { selection_set }
            }
            // Spec: https://spec.graphql.org/draft/#FragmentSpread
            ast::Selection::FragmentSpread(fragment_spread) => {
                let name = fragment_spread
                    .fragment_name()
                    .expect("the node FragmentName is not optional in the spec; qed")
                    .name()
                    .unwrap()
                    .text()
                    .to_string();

                Self::FragmentSpread { name }
            }
        }
    }
}
