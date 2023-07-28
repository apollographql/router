#![cfg_attr(not(test), allow(unused))] // TODO: remove when used

use apollo_compiler::hir;
use apollo_compiler::ApolloCompiler;
use apollo_compiler::HirDatabase;
use tower::BoxError;

/// Transform an executable document with the given visitor.
pub(crate) fn document(
    visitor: &mut impl Visitor,
    file_id: apollo_compiler::FileId,
) -> Result<(), BoxError> {
    for def in visitor.compiler().db.operations(file_id).iter() {
        visitor.operation(def)?;
    }
    for def in visitor.compiler().db.fragments(file_id).values() {
        visitor.fragment_definition(def)?;
    }
    Ok(())
}

pub(crate) trait Visitor: Sized {
    /// A compiler that contains both the executable document to transform
    /// and the corresponding type system definitions.
    fn compiler(&self) -> &ApolloCompiler;

    /// Transform an operation definition.
    ///
    /// Call the [`operation`] free function for the default behavior.
    /// Return `Ok(None)` to remove this operation.
    fn operation(&mut self, hir: &hir::OperationDefinition) -> Result<(), BoxError> {
        operation(self, hir)
    }

    /// Transform a fragment definition.
    ///
    /// Call the [`fragment_definition`] free function for the default behavior.
    /// Return `Ok(None)` to remove this fragment.
    fn fragment_definition(&mut self, hir: &hir::FragmentDefinition) -> Result<(), BoxError> {
        fragment_definition(self, hir)
    }

    /// Transform a field within a selection set.
    ///
    /// Call the [`field`] free function for the default behavior.
    /// Return `Ok(None)` to remove this field.
    fn field(&mut self, parent_type: &str, hir: &hir::Field) -> Result<(), BoxError> {
        field(self, parent_type, hir)
    }

    /// Transform a fragment spread within a selection set.
    ///
    /// Call the [`fragment_spread`] free function for the default behavior.
    /// Return `Ok(None)` to remove this fragment spread.
    fn fragment_spread(&mut self, hir: &hir::FragmentSpread) -> Result<(), BoxError> {
        fragment_spread(self, hir)
    }

    /// Transform a inline fragment within a selection set.
    ///
    /// Call the [`inline_fragment`] free function for the default behavior.
    /// Return `Ok(None)` to remove this inline fragment.
    fn inline_fragment(
        &mut self,
        parent_type: &str,
        hir: &hir::InlineFragment,
    ) -> Result<(), BoxError> {
        inline_fragment(self, parent_type, hir)
    }
}

/// The default behavior for transforming an operation.
///
/// Never returns `Ok(None)`, the `Option` is for `Visitor` impl convenience.
pub(crate) fn operation(
    visitor: &mut impl Visitor,
    def: &hir::OperationDefinition,
) -> Result<(), BoxError> {
    let object_type = def
        .object_type(&visitor.compiler().db)
        .ok_or("ObjectTypeDefMissing")?;
    let type_name = object_type.name();
    selection_set(visitor, def.selection_set(), type_name)?;
    Ok(())
}

/// The default behavior for transforming a fragment definition.
///
/// Never returns `Ok(None)`, the `Option` is for `Visitor` impl convenience.
pub(crate) fn fragment_definition(
    visitor: &mut impl Visitor,
    hir: &hir::FragmentDefinition,
) -> Result<(), BoxError> {
    let type_condition = hir.type_condition();
    selection_set(visitor, hir.selection_set(), type_condition)?;
    Ok(())
}

/// The default behavior for transforming a field within a selection set.
///
/// Returns `Ok(None)` if the field had nested selections and they’re all removed.
pub(crate) fn field(
    visitor: &mut impl Visitor,
    parent_type: &str,
    hir: &hir::Field,
) -> Result<(), BoxError> {
    let name = hir.name();
    let selections = hir.selection_set();
    if !selections.selection().is_empty() {
        let field_type = get_field_type(visitor, parent_type, name)
            .ok_or_else(|| format!("cannot query field '{name}' on type '{parent_type}'"))?;
        selection_set(visitor, selections, &field_type)?
    }
    Ok(())
}

/// The default behavior for transforming a fragment spread.
///
/// Never returns `Ok(None)`, the `Option` is for `Visitor` impl convenience.
pub(crate) fn fragment_spread(
    visitor: &mut impl Visitor,
    hir: &hir::FragmentSpread,
) -> Result<(), BoxError> {
    let _ = (visitor, hir); // Unused, but matches trait method signature
    Ok(())
}

/// The default behavior for transforming an inline fragment.
///
/// Returns `Ok(None)` if all selections within the fragment are removed.
pub(crate) fn inline_fragment(
    visitor: &mut impl Visitor,
    parent_type: &str,
    hir: &hir::InlineFragment,
) -> Result<(), BoxError> {
    let parent_type = hir.type_condition().unwrap_or(parent_type);
    selection_set(visitor, hir.selection_set(), parent_type)?;
    Ok(())
}

fn get_field_type(visitor: &impl Visitor, parent: &str, field_name: &str) -> Option<String> {
    Some(if field_name == "__typename" {
        "String".into()
    } else {
        let db = &visitor.compiler().db;
        db.types_definitions_by_name()
            .get(parent)?
            .field(db, field_name)?
            .ty()
            .name()
    })
}

fn selection_set(
    visitor: &mut impl Visitor,
    hir: &hir::SelectionSet,
    parent_type: &str,
) -> Result<(), BoxError> {
    hir.selection()
        .iter()
        .try_for_each(|hir| selection(visitor, hir, parent_type))
}

fn selection(
    visitor: &mut impl Visitor,
    selection: &hir::Selection,
    parent_type: &str,
) -> Result<(), BoxError> {
    match selection {
        hir::Selection::Field(hir) => visitor.field(parent_type, hir)?,
        hir::Selection::FragmentSpread(hir) => visitor.fragment_spread(hir)?,
        hir::Selection::InlineFragment(hir) => visitor.inline_fragment(parent_type, hir)?,
    }
    Ok(())
}

#[test]
fn test_count_fields() {
    struct CountFields<'a> {
        compiler: &'a ApolloCompiler,
        fields: u32,
    }

    impl<'a> Visitor for CountFields<'a> {
        fn compiler(&self) -> &ApolloCompiler {
            self.compiler
        }

        fn field(&mut self, parent_type: &str, hir: &hir::Field) -> Result<(), BoxError> {
            self.fields += 1;
            field(self, parent_type, hir)
        }
    }

    let mut compiler = ApolloCompiler::new();
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
    let file_id = compiler.add_document(graphql, "");
    let mut visitor = CountFields {
        compiler: &compiler,
        fields: 0,
    };
    document(&mut visitor, file_id).unwrap();
    assert_eq!(visitor.fields, 4)
}
