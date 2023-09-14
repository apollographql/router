#![cfg_attr(not(test), allow(unused))] // TODO: remove when used

use apollo_compiler::hir;
use apollo_compiler::ApolloCompiler;
use apollo_compiler::HirDatabase;
use tower::BoxError;

/// Transform an executable document with the given visitor.
pub(crate) fn document(
    visitor: &mut impl Visitor,
    file_id: apollo_compiler::FileId,
) -> Result<apollo_encoder::Document, BoxError> {
    let mut encoder_node = apollo_encoder::Document::new();

    for def in visitor.compiler().db.operations(file_id).iter() {
        if let Some(op) = visitor.operation(def)? {
            encoder_node.operation(op)
        }
    }
    for def in visitor.compiler().db.fragments(file_id).values() {
        if let Some(f) = visitor.fragment_definition(def)? {
            encoder_node.fragment(f)
        }
    }

    Ok(encoder_node)
}

pub(crate) trait Visitor: Sized {
    /// A compiler that contains both the executable document to transform
    /// and the corresponding type system definitions.
    fn compiler(&self) -> &ApolloCompiler;

    /// Transform an operation definition.
    ///
    /// Call the [`operation`] free function for the default behavior.
    /// Return `Ok(None)` to remove this operation.
    fn operation(
        &mut self,
        hir: &hir::OperationDefinition,
    ) -> Result<Option<apollo_encoder::OperationDefinition>, BoxError> {
        operation(self, hir)
    }

    /// Transform a fragment definition.
    ///
    /// Call the [`fragment_definition`] free function for the default behavior.
    /// Return `Ok(None)` to remove this fragment.
    fn fragment_definition(
        &mut self,
        hir: &hir::FragmentDefinition,
    ) -> Result<Option<apollo_encoder::FragmentDefinition>, BoxError> {
        fragment_definition(self, hir)
    }

    /// Transform a field within a selection set.
    ///
    /// Call the [`field`] free function for the default behavior.
    /// Return `Ok(None)` to remove this field.
    fn field(
        &mut self,
        parent_type: &str,
        hir: &hir::Field,
    ) -> Result<Option<apollo_encoder::Field>, BoxError> {
        field(self, parent_type, hir)
    }

    /// Transform a fragment spread within a selection set.
    ///
    /// Call the [`fragment_spread`] free function for the default behavior.
    /// Return `Ok(None)` to remove this fragment spread.
    fn fragment_spread(
        &mut self,
        hir: &hir::FragmentSpread,
    ) -> Result<Option<apollo_encoder::FragmentSpread>, BoxError> {
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
    ) -> Result<Option<apollo_encoder::InlineFragment>, BoxError> {
        inline_fragment(self, parent_type, hir)
    }

    /// Transform a selection within a selection set.
    ///
    /// Call the [`selection`] free function for the default behavior.
    /// Return `Ok(None)` to remove this selection from the selection set.
    ///
    /// Compared to `field`, `fragment_spread`, or `inline_fragment` trait methods,
    /// this allows returning a different kind of selection.
    fn selection(
        &mut self,
        hir: &hir::Selection,
        parent_type: &str,
    ) -> Result<Option<apollo_encoder::Selection>, BoxError> {
        selection(self, hir, parent_type)
    }
}

/// The default behavior for transforming an operation.
///
/// Never returns `Ok(None)`, the `Option` is for `Visitor` impl convenience.
pub(crate) fn operation(
    visitor: &mut impl Visitor,
    def: &hir::OperationDefinition,
) -> Result<Option<apollo_encoder::OperationDefinition>, BoxError> {
    let object_type = def
        .object_type(&visitor.compiler().db)
        .ok_or("ObjectTypeDefMissing")?;
    let type_name = object_type.name();

    let Some(selection_set) = selection_set(visitor, def.selection_set(), type_name)? else {
        return Ok(None);
    };

    let mut encoder_node =
        apollo_encoder::OperationDefinition::new(operation_type(def.operation_ty()), selection_set);

    if let Some(name) = def.name() {
        encoder_node.name(Some(name.to_string()));
    }

    for def in def.variables() {
        if let Some(v) = variable_definition(def)? {
            encoder_node.variable_definition(v)
        }
    }

    for hir in def.directives() {
        if let Some(d) = directive(hir)? {
            encoder_node.directive(d)
        }
    }

    Ok(Some(encoder_node))
}

/// The default behavior for transforming a fragment definition.
///
/// Never returns `Ok(None)`, the `Option` is for `Visitor` impl convenience.
pub(crate) fn fragment_definition(
    visitor: &mut impl Visitor,
    hir: &hir::FragmentDefinition,
) -> Result<Option<apollo_encoder::FragmentDefinition>, BoxError> {
    let name = hir.name();
    let type_condition = hir.type_condition();

    let Some(selection_set) = selection_set(visitor, hir.selection_set(), type_condition)? else {
        return Ok(None);
    };

    let type_condition = apollo_encoder::TypeCondition::new(type_condition.into());
    let mut encoder_node =
        apollo_encoder::FragmentDefinition::new(name.into(), type_condition, selection_set);
    for hir in hir.directives() {
        if let Some(d) = directive(hir)? {
            encoder_node.directive(d)
        }
    }

    Ok(Some(encoder_node))
}

/// The default behavior for transforming a field within a selection set.
///
/// Returns `Ok(None)` if the field had nested selections and they’re all removed.
pub(crate) fn field(
    visitor: &mut impl Visitor,
    parent_type: &str,
    hir: &hir::Field,
) -> Result<Option<apollo_encoder::Field>, BoxError> {
    let name = hir.name();

    let mut encoder_node = apollo_encoder::Field::new(name.into());

    if let Some(alias) = hir.alias() {
        encoder_node.alias(Some(alias.name().into()));
    }

    for arg in hir.arguments() {
        encoder_node.argument(apollo_encoder::Argument::new(
            arg.name().into(),
            value(arg.value())?,
        ));
    }

    for hir in hir.directives() {
        if let Some(d) = directive(hir)? {
            encoder_node.directive(d)
        }
    }

    let selections = hir.selection_set();
    if !selections.selection().is_empty() {
        let field_type = get_field_type(visitor, parent_type, name)
            .ok_or_else(|| format!("cannot query field '{name}' on type '{parent_type}'"))?;
        match selection_set(visitor, selections, &field_type)? {
            // we expected some fields on that object but got none: that field should be removed
            None => return Ok(None),
            Some(selection_set) => encoder_node.selection_set(Some(selection_set)),
        }
    }

    Ok(Some(encoder_node))
}

/// The default behavior for transforming a fragment spread.
///
/// Never returns `Ok(None)`, the `Option` is for `Visitor` impl convenience.
pub(crate) fn fragment_spread(
    visitor: &mut impl Visitor,
    hir: &hir::FragmentSpread,
) -> Result<Option<apollo_encoder::FragmentSpread>, BoxError> {
    let _ = visitor; // Unused, but matches trait method signature
    let name = hir.name();
    let mut encoder_node = apollo_encoder::FragmentSpread::new(name.into());
    for hir in hir.directives() {
        if let Some(d) = directive(hir)? {
            encoder_node.directive(d)
        }
    }
    Ok(Some(encoder_node))
}

/// The default behavior for transforming an inline fragment.
///
/// Returns `Ok(None)` if all selections within the fragment are removed.
pub(crate) fn inline_fragment(
    visitor: &mut impl Visitor,
    parent_type: &str,
    hir: &hir::InlineFragment,
) -> Result<Option<apollo_encoder::InlineFragment>, BoxError> {
    let parent_type = hir.type_condition().unwrap_or(parent_type);

    let Some(selection_set) = selection_set(visitor, hir.selection_set(), parent_type)? else {
        return Ok(None);
    };

    let mut encoder_node = apollo_encoder::InlineFragment::new(selection_set);

    encoder_node.type_condition(
        hir.type_condition()
            .map(|name| apollo_encoder::TypeCondition::new(name.into())),
    );

    for hir in hir.directives() {
        if let Some(d) = directive(hir)? {
            encoder_node.directive(d)
        }
    }

    Ok(Some(encoder_node))
}

pub(crate) fn get_field_type(
    visitor: &impl Visitor,
    parent: &str,
    field_name: &str,
) -> Option<String> {
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

pub(crate) fn selection_set(
    visitor: &mut impl Visitor,
    hir: &hir::SelectionSet,
    parent_type: &str,
) -> Result<Option<apollo_encoder::SelectionSet>, BoxError> {
    let selections = hir
        .selection()
        .iter()
        .filter_map(|hir| visitor.selection(hir, parent_type).transpose())
        .collect::<Result<Vec<_>, _>>()?;
    Ok((!selections.is_empty()).then(|| apollo_encoder::SelectionSet::with_selections(selections)))
}

pub(crate) fn selection(
    visitor: &mut impl Visitor,
    hir: &hir::Selection,
    parent_type: &str,
) -> Result<Option<apollo_encoder::Selection>, BoxError> {
    Ok(match hir {
        hir::Selection::Field(hir) => visitor
            .field(parent_type, hir)?
            .map(apollo_encoder::Selection::Field),
        hir::Selection::FragmentSpread(hir) => visitor
            .fragment_spread(hir)?
            .map(apollo_encoder::Selection::FragmentSpread),
        hir::Selection::InlineFragment(hir) => visitor
            .inline_fragment(parent_type, hir)?
            .map(apollo_encoder::Selection::InlineFragment),
    })
}

pub(crate) fn variable_definition(
    hir: &hir::VariableDefinition,
) -> Result<Option<apollo_encoder::VariableDefinition>, BoxError> {
    let name = hir.name();
    let ty = ty(hir.ty());

    let mut encoder_node = apollo_encoder::VariableDefinition::new(name.into(), ty);

    if let Some(default_value) = hir.default_value() {
        encoder_node.default_value(value(default_value)?);
    }

    for hir in hir.directives() {
        if let Some(d) = directive(hir)? {
            encoder_node.directive(d)
        }
    }

    Ok(Some(encoder_node))
}

pub(crate) fn directive(
    hir: &hir::Directive,
) -> Result<Option<apollo_encoder::Directive>, BoxError> {
    let name = hir.name().into();
    let mut encoder_directive = apollo_encoder::Directive::new(name);

    for arg in hir.arguments() {
        encoder_directive.arg(apollo_encoder::Argument::new(
            arg.name().into(),
            value(arg.value())?,
        ));
    }

    Ok(Some(encoder_directive))
}

// FIXME: apollo-rs should provide these three conversions, or unify types

pub(crate) fn operation_type(hir: hir::OperationType) -> apollo_encoder::OperationType {
    match hir {
        hir::OperationType::Query => apollo_encoder::OperationType::Query,
        hir::OperationType::Mutation => apollo_encoder::OperationType::Mutation,
        hir::OperationType::Subscription => apollo_encoder::OperationType::Subscription,
    }
}

pub(crate) fn ty(hir: &hir::Type) -> apollo_encoder::Type_ {
    match hir {
        hir::Type::NonNull { ty: hir, .. } => apollo_encoder::Type_::NonNull {
            ty: Box::new(ty(hir)),
        },
        hir::Type::List { ty: hir, .. } => apollo_encoder::Type_::List {
            ty: Box::new(ty(hir)),
        },
        hir::Type::Named { name, .. } => apollo_encoder::Type_::NamedType { name: name.clone() },
    }
}

pub(crate) fn value(hir: &hir::Value) -> Result<apollo_encoder::Value, BoxError> {
    Ok(match hir {
        hir::Value::Variable(val) => apollo_encoder::Value::Variable(val.name().into()),
        hir::Value::Int { value, .. } => value
            .to_i32_checked()
            .map(apollo_encoder::Value::Int)
            .unwrap_or_else(|| apollo_encoder::Value::Float(value.get())),
        hir::Value::Float { value, .. } => apollo_encoder::Value::Float(value.get()),
        hir::Value::String { value, .. } => apollo_encoder::Value::String(value.clone()),
        hir::Value::Boolean { value, .. } => apollo_encoder::Value::Boolean(*value),
        hir::Value::Null { .. } => apollo_encoder::Value::Null,
        hir::Value::Enum { value, .. } => apollo_encoder::Value::Enum(value.src().into()),
        hir::Value::List { value: list, .. } => {
            apollo_encoder::Value::List(list.iter().map(value).collect::<Result<Vec<_>, _>>()?)
        }
        hir::Value::Object { value: obj, .. } => apollo_encoder::Value::Object(
            obj.iter()
                .map(|(k, v)| Ok::<_, BoxError>((k.src().to_string(), value(v)?)))
                .collect::<Result<Vec<_>, _>>()?,
        ),
    })
}

#[test]
fn test_add_directive_to_fields() {
    struct AddDirective<'a>(&'a ApolloCompiler);

    impl<'a> Visitor for AddDirective<'a> {
        fn compiler(&self) -> &ApolloCompiler {
            self.0
        }

        fn field(
            &mut self,
            parent_type: &str,
            hir: &hir::Field,
        ) -> Result<Option<apollo_encoder::Field>, BoxError> {
            Ok(field(self, parent_type, hir)?.map(|mut encoder_node| {
                encoder_node.directive(apollo_encoder::Directive::new("added".into()));
                encoder_node
            }))
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
        }

        fragment F on Query {
            next {
                a
            }
        }
    ";
    let file_id = compiler.add_document(graphql, "");
    let mut visitor = AddDirective(&compiler);
    let expected = "query($id: ID = null) {
  a @added
  ... @defer {
    b @added
  }
  ...F
}
fragment F on Query {
  next @added {
    a @added
  }
}
";
    assert_eq!(
        document(&mut visitor, file_id).unwrap().to_string(),
        expected
    )
}
