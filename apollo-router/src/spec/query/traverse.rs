use apollo_compiler::ast;
use apollo_compiler::schema::FieldLookupError;
use tower::BoxError;

/// Traverse a document with the given visitor.
pub(crate) fn document(
    visitor: &mut impl Visitor,
    document: &ast::Document,
) -> Result<(), BoxError> {
    document.definitions.iter().try_for_each(|def| match def {
        ast::Definition::OperationDefinition(def) => {
            let root_type = visitor
                .schema()
                .root_operation(def.operation_type)
                .ok_or("missing root operation definition")?
                .clone();
            visitor.operation(&root_type, def)
        }
        ast::Definition::FragmentDefinition(def) => visitor.fragment_definition(def),
        _ => Ok(()),
    })
}

pub(crate) trait Visitor: Sized {
    fn schema(&self) -> &apollo_compiler::Schema;

    /// Traverse an operation definition.
    ///
    /// Call the [`operation`] free function for the default behavior.
    /// Return `Ok(None)` to remove this operation.
    fn operation(
        &mut self,
        root_type: &str,
        def: &ast::OperationDefinition,
    ) -> Result<(), BoxError> {
        operation(self, root_type, def)
    }

    /// Traverse a fragment definition.
    ///
    /// Call the [`fragment_definition`] free function for the default behavior.
    /// Return `Ok(None)` to remove this fragment.
    fn fragment_definition(&mut self, def: &ast::FragmentDefinition) -> Result<(), BoxError> {
        fragment_definition(self, def)
    }

    /// Traverse a field within a selection set.
    ///
    /// Call the [`field`] free function for the default behavior.
    /// Return `Ok(None)` to remove this field.
    fn field(
        &mut self,
        _parent_type: &str,
        field_def: &ast::FieldDefinition,
        def: &ast::Field,
    ) -> Result<(), BoxError> {
        field(self, field_def, def)
    }

    /// Traverse a fragment spread within a selection set.
    ///
    /// Call the [`fragment_spread`] free function for the default behavior.
    /// Return `Ok(None)` to remove this fragment spread.
    fn fragment_spread(&mut self, def: &ast::FragmentSpread) -> Result<(), BoxError> {
        fragment_spread(self, def)
    }

    /// Traverse a inline fragment within a selection set.
    ///
    /// Call the [`inline_fragment`] free function for the default behavior.
    /// Return `Ok(None)` to remove this inline fragment.
    fn inline_fragment(
        &mut self,
        parent_type: &str,
        def: &ast::InlineFragment,
    ) -> Result<(), BoxError> {
        inline_fragment(self, parent_type, def)
    }
}

/// The default behavior for traversing an operation.
///
/// Never returns `Ok(None)`, the `Option` is for `Visitor` impl convenience.
pub(crate) fn operation(
    visitor: &mut impl Visitor,
    root_type: &str,
    def: &ast::OperationDefinition,
) -> Result<(), BoxError> {
    selection_set(visitor, root_type, &def.selection_set)
}

/// The default behavior for traversing a fragment definition.
///
/// Never returns `Ok(None)`, the `Option` is for `Visitor` impl convenience.
pub(crate) fn fragment_definition(
    visitor: &mut impl Visitor,
    def: &ast::FragmentDefinition,
) -> Result<(), BoxError> {
    selection_set(visitor, &def.type_condition, &def.selection_set)?;
    Ok(())
}

/// The default behavior for traversing a field within a selection set.
///
/// Returns `Ok(None)` if the field had nested selections and they’re all removed.
pub(crate) fn field(
    visitor: &mut impl Visitor,
    field_def: &ast::FieldDefinition,
    def: &ast::Field,
) -> Result<(), BoxError> {
    selection_set(visitor, field_def.ty.inner_named_type(), &def.selection_set)
}

/// The default behavior for traversing a fragment spread.
///
/// Never returns `Ok(None)`, the `Option` is for `Visitor` impl convenience.
pub(crate) fn fragment_spread(
    visitor: &mut impl Visitor,
    def: &ast::FragmentSpread,
) -> Result<(), BoxError> {
    let _ = (visitor, def); // Unused, but matches trait method signature
    Ok(())
}

/// The default behavior for traversing an inline fragment.
///
/// Returns `Ok(None)` if all selections within the fragment are removed.
pub(crate) fn inline_fragment(
    visitor: &mut impl Visitor,
    parent_type: &str,
    def: &ast::InlineFragment,
) -> Result<(), BoxError> {
    selection_set(visitor, parent_type, &def.selection_set)
}

fn selection_set(
    visitor: &mut impl Visitor,
    parent_type: &str,
    set: &[ast::Selection],
) -> Result<(), BoxError> {
    set.iter().try_for_each(|def| match def {
        ast::Selection::Field(def) => {
            let field_def = &visitor
                .schema()
                .type_field(parent_type, &def.name)
                .map_err(|e| match e {
                    FieldLookupError::NoSuchType => format!("type `{parent_type}` not defined"),
                    FieldLookupError::NoSuchField(_, _) => {
                        format!("no field `{}` in type `{parent_type}`", &def.name)
                    }
                })?
                .clone();
            visitor.field(parent_type, field_def, def)
        }
        ast::Selection::FragmentSpread(def) => visitor.fragment_spread(def),
        ast::Selection::InlineFragment(def) => {
            let fragment_type = def.type_condition.as_deref().unwrap_or(parent_type);
            visitor.inline_fragment(fragment_type, def)
        }
    })
}

#[test]
fn test_count_fields() {
    struct CountFields {
        schema: apollo_compiler::Schema,
        fields: u32,
    }

    impl Visitor for CountFields {
        fn field(
            &mut self,
            _parent_type: &str,
            field_def: &ast::FieldDefinition,
            def: &ast::Field,
        ) -> Result<(), BoxError> {
            self.fields += 1;
            field(self, field_def, def)
        }

        fn schema(&self) -> &apollo_compiler::Schema {
            &self.schema
        }
    }

    let graphql = "
        type Query {
            a: String
            b: Int
            next: Query
        }

        query($id: ID = null) {
            a
            ... @defer {
                b
            }
            ... F
            ... F
        }

        fragment F on Query {
            next {
                a
            }
        }
    ";
    let ast = apollo_compiler::ast::Document::parse(graphql, "");
    let (schema, _doc) = ast.to_mixed();
    let mut visitor = CountFields { fields: 0, schema };
    document(&mut visitor, &ast).unwrap();
    assert_eq!(visitor.fields, 4)
}
