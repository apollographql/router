use std::collections::HashMap;

use apollo_compiler::ast;
use apollo_compiler::schema::FieldLookupError;
use tower::BoxError;

/// Transform a document with the given visitor.
pub(crate) fn document(
    visitor: &mut impl Visitor,
    document: &ast::Document,
) -> Result<ast::Document, BoxError> {
    let mut new = ast::Document {
        sources: document.sources.clone(),
        definitions: Vec::new(),
    };

    // walk through the fragment first: if a fragment is entirely filtered, we want to
    // remove the spread too
    for definition in &document.definitions {
        if let ast::Definition::FragmentDefinition(def) = definition {
            if let Some(new_def) = visitor.fragment_definition(def)? {
                new.definitions
                    .push(ast::Definition::FragmentDefinition(new_def.into()))
            }
        }
    }

    for definition in &document.definitions {
        if let ast::Definition::OperationDefinition(def) = definition {
            let root_type = visitor
                .schema()
                .root_operation(def.operation_type)
                .ok_or("missing root operation definition")?
                .clone();
            if let Some(new_def) = visitor.operation(&root_type, def)? {
                new.definitions
                    .push(ast::Definition::OperationDefinition(new_def.into()))
            }
        }
    }
    Ok(new)
}

pub(crate) trait Visitor: Sized {
    fn schema(&self) -> &apollo_compiler::Schema;

    /// Transform an operation definition.
    ///
    /// Call the [`operation`] free function for the default behavior.
    /// Return `Ok(None)` to remove this operation.
    fn operation(
        &mut self,
        root_type: &str,
        def: &ast::OperationDefinition,
    ) -> Result<Option<ast::OperationDefinition>, BoxError> {
        operation(self, root_type, def)
    }

    /// Transform a fragment definition.
    ///
    /// Call the [`fragment_definition`] free function for the default behavior.
    /// Return `Ok(None)` to remove this fragment.
    fn fragment_definition(
        &mut self,
        def: &ast::FragmentDefinition,
    ) -> Result<Option<ast::FragmentDefinition>, BoxError> {
        fragment_definition(self, def)
    }

    /// Transform a field within a selection set.
    ///
    /// Call the [`field`] free function for the default behavior.
    /// Return `Ok(None)` to remove this field.
    fn field(
        &mut self,
        _parent_type: &str,
        field_def: &ast::FieldDefinition,
        def: &ast::Field,
    ) -> Result<Option<ast::Field>, BoxError> {
        field(self, field_def, def)
    }

    /// Transform a fragment spread within a selection set.
    ///
    /// Call the [`fragment_spread`] free function for the default behavior.
    /// Return `Ok(None)` to remove this fragment spread.
    fn fragment_spread(
        &mut self,
        def: &ast::FragmentSpread,
    ) -> Result<Option<ast::FragmentSpread>, BoxError> {
        fragment_spread(self, def)
    }

    /// Transform a inline fragment within a selection set.
    ///
    /// Call the [`inline_fragment`] free function for the default behavior.
    /// Return `Ok(None)` to remove this inline fragment.
    fn inline_fragment(
        &mut self,
        parent_type: &str,
        def: &ast::InlineFragment,
    ) -> Result<Option<ast::InlineFragment>, BoxError> {
        inline_fragment(self, parent_type, def)
    }
}

/// The default behavior for transforming an operation.
///
/// Never returns `Ok(None)`, the `Option` is for `Visitor` impl convenience.
pub(crate) fn operation(
    visitor: &mut impl Visitor,
    root_type: &str,
    def: &ast::OperationDefinition,
) -> Result<Option<ast::OperationDefinition>, BoxError> {
    let Some(selection_set) = selection_set(visitor, root_type, &def.selection_set)? else {
        return Ok(None);
    };

    Ok(Some(ast::OperationDefinition {
        name: def.name.clone(),
        operation_type: def.operation_type,
        variables: def.variables.clone(),
        directives: def.directives.clone(),
        selection_set,
    }))
}

/// The default behavior for transforming a fragment definition.
///
/// Never returns `Ok(None)`, the `Option` is for `Visitor` impl convenience.
pub(crate) fn fragment_definition(
    visitor: &mut impl Visitor,
    def: &ast::FragmentDefinition,
) -> Result<Option<ast::FragmentDefinition>, BoxError> {
    let Some(selection_set) = selection_set(visitor, &def.type_condition, &def.selection_set)?
    else {
        return Ok(None);
    };
    Ok(Some(ast::FragmentDefinition {
        name: def.name.clone(),
        type_condition: def.type_condition.clone(),
        directives: def.directives.clone(),
        selection_set,
    }))
}

/// The default behavior for transforming a field within a selection set.
///
/// Returns `Ok(None)` if the field had nested selections and they’re all removed.
pub(crate) fn field(
    visitor: &mut impl Visitor,
    field_def: &ast::FieldDefinition,
    def: &ast::Field,
) -> Result<Option<ast::Field>, BoxError> {
    let Some(selection_set) =
        selection_set(visitor, field_def.ty.inner_named_type(), &def.selection_set)?
    else {
        return Ok(None);
    };
    Ok(Some(ast::Field {
        alias: def.alias.clone(),
        name: def.name.clone(),
        arguments: def.arguments.clone(),
        directives: def.directives.clone(),
        selection_set,
    }))
}

/// The default behavior for transforming a fragment spread.
///
/// Never returns `Ok(None)`, the `Option` is for `Visitor` impl convenience.
pub(crate) fn fragment_spread(
    visitor: &mut impl Visitor,
    def: &ast::FragmentSpread,
) -> Result<Option<ast::FragmentSpread>, BoxError> {
    let _ = visitor; // Unused, but matches trait method signature
    Ok(Some(def.clone()))
}

/// The default behavior for transforming an inline fragment.
///
/// Returns `Ok(None)` if all selections within the fragment are removed.
pub(crate) fn inline_fragment(
    visitor: &mut impl Visitor,
    parent_type: &str,
    def: &ast::InlineFragment,
) -> Result<Option<ast::InlineFragment>, BoxError> {
    let Some(selection_set) = selection_set(visitor, parent_type, &def.selection_set)? else {
        return Ok(None);
    };
    Ok(Some(ast::InlineFragment {
        type_condition: def.type_condition.clone(),
        directives: def.directives.clone(),
        selection_set,
    }))
}

pub(crate) fn selection_set(
    visitor: &mut impl Visitor,
    parent_type: &str,
    set: &[ast::Selection],
) -> Result<Option<Vec<ast::Selection>>, BoxError> {
    if set.is_empty() {
        return Ok(Some(Vec::new()));
    }
    let mut selections = Vec::new();
    for sel in set {
        match sel {
            ast::Selection::Field(def) => {
                let field_def = visitor
                    .schema()
                    .type_field(parent_type, &def.name)
                    .map_err(|e| match e {
                        FieldLookupError::NoSuchType => format!("type `{parent_type}` not defined"),
                        FieldLookupError::NoSuchField(_, _) => {
                            format!("no field `{}` in type `{parent_type}`", &def.name)
                        }
                    })?
                    .clone();
                if let Some(sel) = visitor.field(parent_type, &field_def, def)? {
                    selections.push(ast::Selection::Field(sel.into()))
                }
            }
            ast::Selection::FragmentSpread(def) => {
                if let Some(sel) = visitor.fragment_spread(def)? {
                    selections.push(ast::Selection::FragmentSpread(sel.into()))
                }
            }
            ast::Selection::InlineFragment(def) => {
                let fragment_type = def
                    .type_condition
                    .as_ref()
                    .map(|s| s.as_str())
                    .unwrap_or(parent_type);
                if let Some(sel) = visitor.inline_fragment(fragment_type, def)? {
                    selections.push(ast::Selection::InlineFragment(sel.into()))
                }
            }
        }
    }
    Ok((!selections.is_empty()).then_some(selections))
}

pub(crate) fn collect_fragments(
    executable: &ast::Document,
) -> HashMap<&ast::Name, &ast::FragmentDefinition> {
    executable
        .definitions
        .iter()
        .filter_map(|def| match def {
            ast::Definition::FragmentDefinition(frag) => Some((&frag.name, frag.as_ref())),
            _ => None,
        })
        .collect()
}

#[test]
fn test_add_directive_to_fields() {
    struct AddDirective {
        schema: apollo_compiler::Schema,
    }

    impl Visitor for AddDirective {
        fn field(
            &mut self,
            _parent_type: &str,
            field_def: &ast::FieldDefinition,
            def: &ast::Field,
        ) -> Result<Option<ast::Field>, BoxError> {
            Ok(field(self, field_def, def)?.map(|mut new| {
                new.directives.push(
                    ast::Directive {
                        name: apollo_compiler::name!("added"),
                        arguments: Vec::new(),
                    }
                    .into(),
                );
                new
            }))
        }

        fn schema(&self) -> &apollo_compiler::Schema {
            &self.schema
        }
    }

    let graphql = "
        type Query {
            a(id: ID): String
            b: Int
            next: Query
        }
        directive @defer(label: String, if: Boolean! = true) on FRAGMENT_SPREAD | INLINE_FRAGMENT

        query($id: ID = null) {
            a(id: $id)
            ... @defer {
                b
            }
            ... F
        }

        fragment F on Query {
            next {
                a
            }
        }
    ";
    let ast = apollo_compiler::ast::Document::parse(graphql, "").unwrap();
    let (schema, _doc) = ast.to_mixed_validate().unwrap();
    let schema = schema.into_inner();
    let mut visitor = AddDirective { schema };
    let expected = "fragment F on Query {
  next @added {
    a @added
  }
}

query($id: ID = null) {
  a(id: $id) @added
  ... @defer {
    b @added
  }
  ...F
}
";
    assert_eq!(document(&mut visitor, &ast).unwrap().to_string(), expected)
}
