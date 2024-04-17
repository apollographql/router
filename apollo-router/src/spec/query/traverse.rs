use apollo_compiler::ast;
use apollo_compiler::executable;
use apollo_compiler::schema::FieldLookupError;
use apollo_compiler::ExecutableDocument;
use tower::BoxError;

/// Traverse a document with the given visitor.
pub(crate) fn document(
    visitor: &mut impl Visitor,
    document: &ExecutableDocument,
    operation_name: Option<&str>,
) -> Result<(), BoxError> {
    if let Ok(operation) = document.get_operation(operation_name) {
        visitor.operation(operation.object_type().as_str(), operation)?;
    }

    for fragment in document.fragments.values() {
        visitor.fragment(fragment)?;
    }

    Ok(())
}

pub(crate) trait Visitor: Sized {
    fn schema(&self) -> &apollo_compiler::Schema;

    /// Traverse an operation definition.
    ///
    /// Call the [`operation`] free function for the default behavior.
    /// Return `Ok(None)` to remove this operation.
    fn operation(&mut self, root_type: &str, def: &executable::Operation) -> Result<(), BoxError> {
        operation(self, root_type, def)
    }

    /// Traverse a fragment definition.
    ///
    /// Call the [`fragment_definition`] free function for the default behavior.
    /// Return `Ok(None)` to remove this fragment.
    fn fragment(&mut self, def: &executable::Fragment) -> Result<(), BoxError> {
        fragment(self, def)
    }

    /// Traverse a field within a selection set.
    ///
    /// Call the [`field`] free function for the default behavior.
    /// Return `Ok(None)` to remove this field.
    fn field(
        &mut self,
        _parent_type: &str,
        field_def: &ast::FieldDefinition,
        def: &executable::Field,
    ) -> Result<(), BoxError> {
        field(self, field_def, def)
    }

    /// Traverse a fragment spread within a selection set.
    ///
    /// Call the [`fragment_spread`] free function for the default behavior.
    /// Return `Ok(None)` to remove this fragment spread.
    fn fragment_spread(&mut self, def: &executable::FragmentSpread) -> Result<(), BoxError> {
        fragment_spread(self, def)
    }

    /// Traverse a inline fragment within a selection set.
    ///
    /// Call the [`inline_fragment`] free function for the default behavior.
    /// Return `Ok(None)` to remove this inline fragment.
    fn inline_fragment(
        &mut self,
        parent_type: &str,
        def: &executable::InlineFragment,
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
    def: &executable::Operation,
) -> Result<(), BoxError> {
    //FIXME: we should look at directives etc on operation
    selection_set(visitor, root_type, &def.selection_set.selections)
}

/// The default behavior for traversing a fragment definition.
///
/// Never returns `Ok(None)`, the `Option` is for `Visitor` impl convenience.
pub(crate) fn fragment(
    visitor: &mut impl Visitor,
    def: &executable::Fragment,
) -> Result<(), BoxError> {
    selection_set(visitor, def.type_condition(), &def.selection_set.selections)?;
    Ok(())
}

/// The default behavior for traversing a field within a selection set.
///
/// Returns `Ok(None)` if the field had nested selections and they’re all removed.
pub(crate) fn field(
    visitor: &mut impl Visitor,
    field_def: &ast::FieldDefinition,
    def: &executable::Field,
) -> Result<(), BoxError> {
    selection_set(
        visitor,
        field_def.ty.inner_named_type(),
        &def.selection_set.selections,
    )
}

/// The default behavior for traversing a fragment spread.
///
/// Never returns `Ok(None)`, the `Option` is for `Visitor` impl convenience.
pub(crate) fn fragment_spread(
    visitor: &mut impl Visitor,
    def: &executable::FragmentSpread,
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
    def: &executable::InlineFragment,
) -> Result<(), BoxError> {
    selection_set(visitor, parent_type, &def.selection_set.selections)
}

pub(crate) fn selection_set(
    visitor: &mut impl Visitor,
    parent_type: &str,
    set: &[executable::Selection],
) -> Result<(), BoxError> {
    set.iter().try_for_each(|def| match def {
        executable::Selection::Field(def) => {
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
        executable::Selection::FragmentSpread(def) => visitor.fragment_spread(def),
        executable::Selection::InlineFragment(def) => {
            let fragment_type = def
                .type_condition
                .as_ref()
                .map(|s| s.as_str())
                .unwrap_or(parent_type);
            visitor.inline_fragment(fragment_type, def)
        }
    })
}

#[test]
fn test_count_fields() {
    use apollo_compiler::validation::Valid;

    struct CountFields {
        schema: apollo_compiler::Schema,
        fields: u32,
    }

    impl Visitor for CountFields {
        fn field(
            &mut self,
            _parent_type: &str,
            field_def: &ast::FieldDefinition,
            def: &executable::Field,
        ) -> Result<(), BoxError> {
            self.fields += 1;
            field(self, field_def, def)
        }

        fn schema(&self) -> &apollo_compiler::Schema {
            &self.schema
        }
    }

    let schema = "
    type Query {
        a(id: ID): String
        b: Int
        next: Query
    }
    directive @defer(label: String, if: Boolean! = true) on FRAGMENT_SPREAD | INLINE_FRAGMENT";
    let query = "
        query($id: ID = null) {
            a(id: $id)
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
    let ast = apollo_compiler::ast::Document::parse(schema, "").unwrap();
    let schema = ast.to_schema_validate().unwrap();
    let schema = schema.into_inner();
    let executable =
        ExecutableDocument::parse(Valid::assume_valid_ref(&schema), query, "").unwrap();
    let mut visitor = CountFields { fields: 0, schema };
    document(&mut visitor, &executable, None).unwrap();
    assert_eq!(visitor.fields, 4)
}
