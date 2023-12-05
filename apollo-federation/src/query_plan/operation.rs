use apollo_compiler::ast::{Argument, DirectiveList, FieldDefinition, Name, NamedType};
use apollo_compiler::executable::{
    Field, Fragment, InlineFragment, Operation, Selection, SelectionSet,
};
use apollo_compiler::{Node, Schema};
use indexmap::map::Entry;
use indexmap::IndexMap;

// copy of apollo compiler types that store selections in a map so we can normalize it efficiently
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedSelectionSet {
    pub ty: NamedType,
    pub selections: NormalizedSelectionMap,
}

/// Normalized selection map is an optimized representation of a regular SelectionSet which does not
/// contains any duplicated entries and optimizes fragment usages. By storing selection set as a map,
/// we can efficiently join multiple selection sets.
pub type NormalizedSelectionMap = IndexMap<NormalizedSelectionKey, NormalizedSelection>;

/// Unique identifier of a selection that is used to determine whether fields/fragments can be merged.
///
/// In order to merge two selections they need to
/// * reference the same field/inline fragment
/// * specify the same directives
/// * directives have to be applied in the same order
/// * directive arguments order does not matter (they get automatically sorted by their names).
/// * selection cannot specify @defer directive
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum NormalizedSelectionKey {
    Field {
        // field alias (if specified) or field name in the resulting selection set
        response_name: Name,
        // directive applied on the field
        directives: DirectiveList,
        // unique label/counter used to distinguish fields that cannot be merged
        label: i32,
    },
    InlineFragment {
        // optional type condition of a fragment
        type_condition: Option<Name>,
        // directives applied on a fragment
        directives: DirectiveList,
        // unique label/counter used to distinguish fragments that cannot be merged
        label: i32,
    },
}

// copy of apollo compiler types that store selections in a map so we can normalize it efficiently
// we no longer have FragmentSpread variant as they get either auto expanded and merged into regular
// field selection or converted to inline fragments if we cannot merge them
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NormalizedSelection {
    NormalizedField(Node<NormalizedField>),
    NormalizedInlineFragment(Node<NormalizedInlineFragment>),
}

// copy of apollo compiler types that store selections in a map so we can normalize it efficiently
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedField {
    pub definition: Node<FieldDefinition>,
    pub alias: Option<Name>,
    pub name: Name,
    pub arguments: Vec<Node<Argument>>,
    pub directives: DirectiveList,
    pub selection_set: NormalizedSelectionSet,
}

// copy of apollo compiler types that store selections in a map so we can normalize it efficiently
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedInlineFragment {
    pub type_condition: Option<NamedType>,
    pub directives: DirectiveList,
    pub selection_set: NormalizedSelectionSet,
}

impl NormalizedSelectionSet {
    fn from_selection_set(
        selection_set: &SelectionSet,
        fragments: &IndexMap<Name, Node<Fragment>>,
    ) -> Self {
        let normalized_selections =
            normalize_selections(&selection_set.selections, &selection_set.ty, fragments);
        NormalizedSelectionSet {
            ty: selection_set.ty.clone(),
            selections: normalized_selections,
        }
    }
}

impl From<NormalizedSelectionSet> for SelectionSet {
    fn from(val: NormalizedSelectionSet) -> Self {
        SelectionSet {
            ty: val.ty.clone(),
            selections: flatten_selections(&val.selections),
        }
    }
}

impl NormalizedSelectionKey {
    /// Generate new key by incrementing unique label value
    fn next_key(self) -> Self {
        match self {
            Self::Field {
                response_name,
                directives,
                label,
            } => Self::Field {
                response_name: response_name.clone(),
                directives: directives.clone(),
                label: label + 1,
            },
            Self::InlineFragment {
                type_condition,
                directives,
                label,
            } => Self::InlineFragment {
                type_condition: type_condition.clone(),
                directives: directives.clone(),
                label: label + 1,
            },
        }
    }
}

impl From<&'_ Node<Field>> for NormalizedSelectionKey {
    fn from(field: &'_ Node<Field>) -> Self {
        Self::Field {
            response_name: field.alias.clone().unwrap_or_else(|| field.name.clone()),
            directives: directives_with_sorted_arguments(&field.directives),
            label: 0,
        }
    }
}

impl From<&'_ Node<Fragment>> for NormalizedSelectionKey {
    fn from(fragment: &'_ Node<Fragment>) -> Self {
        Self::InlineFragment {
            type_condition: Some(fragment.type_condition().clone()),
            directives: directives_with_sorted_arguments(&fragment.directives),
            label: 0,
        }
    }
}

impl From<&'_ Node<InlineFragment>> for NormalizedSelectionKey {
    fn from(inline_fragment: &'_ Node<InlineFragment>) -> Self {
        Self::InlineFragment {
            type_condition: inline_fragment.type_condition.clone(),
            directives: directives_with_sorted_arguments(&inline_fragment.directives),
            label: 0,
        }
    }
}

impl From<&'_ Node<NormalizedField>> for NormalizedSelectionKey {
    fn from(field: &'_ Node<NormalizedField>) -> Self {
        Self::Field {
            response_name: field.alias.clone().unwrap_or_else(|| field.name.clone()),
            directives: field.directives.clone(),
            label: 0,
        }
    }
}

impl From<&mut Node<NormalizedInlineFragment>> for NormalizedSelectionKey {
    fn from(inline_fragment: &mut Node<NormalizedInlineFragment>) -> Self {
        Self::InlineFragment {
            type_condition: inline_fragment.type_condition.clone(),
            directives: inline_fragment.directives.clone(),
            label: 0,
        }
    }
}

fn directives_with_sorted_arguments(directives: &DirectiveList) -> DirectiveList {
    let mut directives = directives.clone();
    for directive in &mut directives {
        directive
            .make_mut()
            .arguments
            .sort_by(|a1, a2| a1.name.cmp(&a2.name))
    }
    directives
}

/// Converts vec of Selections to a map of NormalizedSelections.
///
/// Performs following normalizations
/// * expands all named fragments
/// * if possible merge fragments to regular field selections (if not possible to merge named fragment,
///  it will be converted to inline fragment)
/// * merge duplicate field selections
/// * removes all __schema/__type introspection fields selections.
fn normalize_selections(
    selections: &Vec<Selection>,
    parent_type: &NamedType,
    fragments: &IndexMap<Name, Node<Fragment>>,
) -> NormalizedSelectionMap {
    let mut normalized = NormalizedSelectionMap::new();
    for selection in selections {
        match selection {
            Selection::Field(field) => {
                // skip __schema/__type introspection fields as they are not used for query planning
                if field.name == "__schema" || field.name == "__type" {
                    continue;
                }

                let mut fragment_key: NormalizedSelectionKey = field.into();
                // deferred fields should not be merged
                let is_deferred = is_deferred_selection(&field.directives);
                if is_deferred {
                    while normalized.contains_key(&fragment_key) {
                        fragment_key = fragment_key.next_key();
                    }
                }

                let normalized_field = normalized.entry(fragment_key).or_insert_with(|| {
                    NormalizedSelection::NormalizedField(Node::new(NormalizedField {
                        definition: field.definition.clone(),
                        alias: field.alias.clone(),
                        name: field.name.clone(),
                        arguments: field.arguments.clone(),
                        directives: field.directives.clone(),
                        selection_set: NormalizedSelectionSet {
                            ty: field.selection_set.ty.clone(),
                            selections: IndexMap::new(),
                        },
                    }))
                });
                if let NormalizedSelection::NormalizedField(field_entry) = normalized_field {
                    let expanded_selection_set = normalize_selections(
                        &field.selection_set.selections,
                        field.ty().inner_named_type(),
                        fragments,
                    );
                    let merged_selections = merge_selections(
                        &field_entry.selection_set.selections,
                        &expanded_selection_set,
                    );
                    field_entry.make_mut().selection_set.selections = merged_selections;
                }
            }
            Selection::FragmentSpread(named_fragment) => {
                if let Some(fragment) = fragments.get(&named_fragment.fragment_name) {
                    let expanded_selection_set = normalize_selections(
                        &fragment.selection_set.selections,
                        parent_type,
                        fragments,
                    );

                    // we can collapse named fragments if condition is on the parent type and we don't have any directives
                    if parent_type == fragment.type_condition() && fragment.directives.is_empty() {
                        normalized = merge_selections(&normalized, &expanded_selection_set);
                    } else {
                        // otherwise we convert to inline fragment
                        let mut fragment_key: NormalizedSelectionKey = fragment.into();
                        // deferred fragments should not be merged
                        let is_deferred = is_deferred_selection(&fragment.directives);
                        if is_deferred {
                            while normalized.contains_key(&fragment_key) {
                                fragment_key = fragment_key.next_key();
                            }
                        }
                        if let NormalizedSelection::NormalizedInlineFragment(fragment_entry) =
                            normalized.entry(fragment_key).or_insert_with(|| {
                                NormalizedSelection::NormalizedInlineFragment(Node::new(
                                    NormalizedInlineFragment {
                                        type_condition: Some(fragment.type_condition().clone()),
                                        directives: fragment.directives.clone(),
                                        selection_set: NormalizedSelectionSet {
                                            ty: fragment.selection_set.ty.clone(),
                                            selections: IndexMap::new(),
                                        },
                                    },
                                ))
                            })
                        {
                            let merged_selections = merge_selections(
                                &fragment_entry.selection_set.selections,
                                &expanded_selection_set,
                            );
                            fragment_entry.make_mut().selection_set.selections = merged_selections;
                        }
                    }
                } else {
                    // no fragment found - should never happen as it would be invalid operation
                }
            }
            Selection::InlineFragment(inline_fragment) => {
                let expanded_selection_set = normalize_selections(
                    &inline_fragment.selection_set.selections,
                    parent_type,
                    fragments,
                );
                // we can collapse selection set if inline fragment condition is on the parent type and we don't have any directives
                if Some(parent_type) == inline_fragment.type_condition.as_ref()
                    && inline_fragment.directives.is_empty()
                {
                    normalized = merge_selections(&normalized, &expanded_selection_set);
                } else {
                    let mut fragment_key: NormalizedSelectionKey = inline_fragment.into();
                    // deferred fragments should not be merged
                    let is_deferred = is_deferred_selection(&inline_fragment.directives);
                    if is_deferred {
                        while normalized.contains_key(&fragment_key) {
                            fragment_key = fragment_key.next_key();
                        }
                    }
                    if let NormalizedSelection::NormalizedInlineFragment(fragment_entry) =
                        normalized.entry(fragment_key).or_insert_with(|| {
                            NormalizedSelection::NormalizedInlineFragment(Node::new(
                                NormalizedInlineFragment {
                                    type_condition: inline_fragment.type_condition.clone(),
                                    directives: inline_fragment.directives.clone(),
                                    selection_set: NormalizedSelectionSet {
                                        ty: inline_fragment.selection_set.ty.clone(),
                                        selections: IndexMap::new(),
                                    },
                                },
                            ))
                        })
                    {
                        let merged_selections = merge_selections(
                            &fragment_entry.selection_set.selections,
                            &expanded_selection_set,
                        );
                        fragment_entry.make_mut().selection_set.selections = merged_selections;
                    }
                }
            }
        }
    }
    normalized
}

fn merge_selections(
    source: &NormalizedSelectionMap,
    to_merge: &NormalizedSelectionMap,
) -> NormalizedSelectionMap {
    let mut merged_selections = source.clone();
    for (key, selection) in to_merge {
        match merged_selections.entry(key.clone()) {
            Entry::Occupied(mut entry) => {
                match entry.get_mut() {
                    NormalizedSelection::NormalizedField(ref mut source_field) => {
                        if let NormalizedSelection::NormalizedField(field_to_merge) = selection {
                            let mut field_key: NormalizedSelectionKey = field_to_merge.into();
                            let is_deferred = is_deferred_selection(&field_to_merge.directives);
                            if field_to_merge.name != source_field.name
                                || field_to_merge.definition.ty != source_field.definition.ty
                            {
                                panic!("TODO invalid operation");
                            }
                            if is_deferred {
                                while merged_selections.contains_key(&field_key) {
                                    field_key = field_key.next_key();
                                }
                                // insert new
                                merged_selections.insert(
                                    field_key,
                                    NormalizedSelection::NormalizedField(field_to_merge.clone()),
                                );
                            } else {
                                let merged_field_selections = merge_selections(
                                    &source_field.selection_set.selections,
                                    &field_to_merge.selection_set.selections,
                                );
                                let merged_selection_set = NormalizedSelectionSet {
                                    ty: source_field.selection_set.ty.clone(),
                                    selections: merged_field_selections,
                                };
                                source_field.make_mut().selection_set = merged_selection_set;
                            }
                        }
                    }
                    NormalizedSelection::NormalizedInlineFragment(ref mut source_fragment) => {
                        if let NormalizedSelection::NormalizedInlineFragment(fragment_to_merge) =
                            selection
                        {
                            let mut fragment_key: NormalizedSelectionKey = source_fragment.into();
                            // deferred fragments should not be merged
                            let is_deferred = is_deferred_selection(&fragment_to_merge.directives);
                            if is_deferred {
                                while merged_selections.contains_key(&fragment_key) {
                                    fragment_key = fragment_key.next_key();
                                }
                                // insert new
                                merged_selections.insert(
                                    fragment_key,
                                    NormalizedSelection::NormalizedInlineFragment(
                                        fragment_to_merge.clone(),
                                    ),
                                );
                            } else {
                                // can merge
                                let merged_fragment_selections = merge_selections(
                                    &source_fragment.selection_set.selections,
                                    &fragment_to_merge.selection_set.selections,
                                );
                                let merged_selection_set = NormalizedSelectionSet {
                                    ty: source_fragment.selection_set.ty.clone(),
                                    selections: merged_fragment_selections,
                                };
                                source_fragment.make_mut().selection_set = merged_selection_set;
                            }
                        }
                    }
                }
            }
            Entry::Vacant(entry) => {
                entry.insert(selection.clone());
            }
        }
    }
    merged_selections
}

fn is_deferred_selection(directives: &DirectiveList) -> bool {
    directives.iter().any(|d| d.name == "defer")
}

/// Converts NormalizedSelectionMap back to Vec of Selections.
fn flatten_selections(selections: &NormalizedSelectionMap) -> Vec<Selection> {
    let mut flattened = vec![];
    for selection in selections.values() {
        match selection {
            NormalizedSelection::NormalizedField(normalized_field) => {
                let selections = flatten_selections(&normalized_field.selection_set.selections);
                let field = Field {
                    definition: normalized_field.definition.to_owned(),
                    alias: normalized_field.alias.to_owned(),
                    name: normalized_field.name.to_owned(),
                    arguments: normalized_field.arguments.to_owned(),
                    directives: normalized_field.directives.to_owned(),
                    selection_set: SelectionSet {
                        ty: normalized_field.selection_set.ty.clone(),
                        selections,
                    },
                };
                flattened.push(Selection::Field(Node::new(field)));
            }
            NormalizedSelection::NormalizedInlineFragment(normalized_fragment) => {
                let selections = flatten_selections(&normalized_fragment.selection_set.selections);
                let fragment = InlineFragment {
                    type_condition: normalized_fragment.type_condition.to_owned(),
                    directives: normalized_fragment.directives.to_owned(),
                    selection_set: SelectionSet {
                        ty: normalized_fragment.selection_set.ty.clone(),
                        selections,
                    },
                };
                flattened.push(Selection::InlineFragment(Node::new(fragment)));
            }
        }
    }
    flattened
}

/// Normalizes selection set within specified operation.
///
/// This method applies following normalizations
/// - expands all fragments within an operation
/// - merge same selections
/// - removes all introspection fields from top-level selection
/// - attempts to remove all unnecessary/redundant inline fragments
pub fn normalize_operation(
    operation: &mut Operation,
    _schema: &Schema,
    fragments: &IndexMap<Name, Node<Fragment>>,
) {
    let normalized_selection_set =
        NormalizedSelectionSet::from_selection_set(&operation.selection_set, fragments);

    // flatten back to vec
    operation.selection_set = SelectionSet::from(normalized_selection_set);
}

#[cfg(test)]
mod tests {
    use crate::query_plan::operation::normalize_operation;

    #[test]
    fn expands_named_fragments() {
        let operation_with_named_fragment = r#"
query NamedFragmentQuery {
  foo {
    id
    ...Bar
  }
}

fragment Bar on Foo {
  bar
  baz
}

type Query {
  foo: Foo
}

type Foo {
  id: ID!
  bar: String!
  baz: Int
}
"#;
        let (schema, executable_document) = apollo_compiler::parse_mixed_validate(
            operation_with_named_fragment,
            "document.graphql",
        )
        .unwrap();
        let mut executable_document = executable_document.into_inner();
        if let Some(operation) = executable_document
            .named_operations
            .get_mut("NamedFragmentQuery")
        {
            let operation = operation.make_mut();
            normalize_operation(operation, &schema, &executable_document.fragments);

            let expected = r#"query NamedFragmentQuery {
  foo {
    id
    bar
    baz
  }
}"#;
            let actual = operation.to_string();
            assert_eq!(expected, actual);
        }
    }

    #[test]
    fn expands_and_deduplicates_fragments() {
        let operation_with_named_fragment = r#"
query NestedFragmentQuery {
  foo {
    ...FirstFragment
    ...SecondFragment
  }
}

fragment FirstFragment on Foo {
  id
  bar
  baz
}

fragment SecondFragment on Foo {
  id
  bar
}

type Query {
  foo: Foo
}

type Foo {
  id: ID!
  bar: String!
  baz: String
}
"#;
        let (schema, executable_document) = apollo_compiler::parse_mixed_validate(
            operation_with_named_fragment,
            "document.graphql",
        )
        .unwrap();
        let mut executable_document = executable_document.into_inner();
        if let Some((_, operation)) = executable_document.named_operations.first_mut() {
            let operation = operation.make_mut();
            normalize_operation(operation, &schema, &executable_document.fragments);

            let expected = r#"query NestedFragmentQuery {
  foo {
    id
    bar
    baz
  }
}"#;
            let actual = format!("{}", operation);
            assert_eq!(expected, actual);
        }
    }

    #[test]
    fn can_remove_introspection_selections() {
        let operation_with_introspection = r#"
query TestIntrospectionQuery {
  __schema {
    types {
      name
    }
  }
}

type Query {
  foo: String
}
"#;
        let (schema, executable_document) =
            apollo_compiler::parse_mixed_validate(operation_with_introspection, "document.graphql")
                .unwrap();
        let mut executable_document = executable_document.into_inner();
        if let Some(operation) = executable_document
            .named_operations
            .get_mut("TestIntrospectionQuery")
        {
            let operation = operation.make_mut();
            normalize_operation(operation, &schema, &executable_document.fragments);

            assert!(operation.selection_set.selections.is_empty());
        }
    }
}
