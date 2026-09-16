//! The schema queries the inclusion checker needs, precomputed once per comparison.
//!
//! Port of the `GraphQL.Schema` helpers used by `QueryInclusion`: `getPossibleTypes`,
//! `lookupField`, and `TypeRef.isCompositeBool`. `getPossibleTypes` is on the hot path — it runs
//! for every inline fragment and every type region — so possible-type sets are materialized up
//! front rather than recomputed from the referencers map on each call.

use apollo_compiler::Name;
use apollo_compiler::ast;
use apollo_compiler::collections::IndexMap;
use apollo_compiler::schema::ExtendedType;

use crate::FederationError;
use crate::schema::ValidFederationSchema;
use crate::schema::position::CompositeTypeDefinitionPosition;

pub(crate) struct SchemaView<'schema> {
    schema: &'schema ValidFederationSchema,
    /// Possible object types per composite type name, sorted by name.
    ///
    /// The Lean model returns these in schema declaration order. Sorting instead keeps regions
    /// canonical regardless of how they were reached, which is what the child-task de-duplication
    /// relies on; the verdict does not depend on the order either way, since regions are sets.
    possible_types: IndexMap<Name, Vec<Name>>,
    empty: Vec<Name>,
}

impl<'schema> SchemaView<'schema> {
    pub(crate) fn new(schema: &'schema ValidFederationSchema) -> Result<Self, FederationError> {
        let mut possible_types = IndexMap::default();
        for (name, ty) in schema.schema().types.iter() {
            let position: CompositeTypeDefinitionPosition = match ty {
                ExtendedType::Object(_) | ExtendedType::Interface(_) | ExtendedType::Union(_) => {
                    schema.get_type(name)?.try_into()?
                }
                _ => continue,
            };
            let mut types: Vec<Name> = schema
                .possible_runtime_types(position)?
                .into_iter()
                .map(|ty| ty.type_name)
                .collect();
            types.sort();
            possible_types.insert(name.clone(), types);
        }
        Ok(SchemaView {
            schema,
            possible_types,
            empty: Vec::new(),
        })
    }

    /// The object types a composite type can resolve to. Empty for leaf types and unknown names,
    /// matching the Lean `getPossibleTypes`.
    pub(crate) fn possible_types(&self, type_name: &Name) -> &[Name] {
        self.possible_types.get(type_name).unwrap_or(&self.empty)
    }

    /// The definition of `field_name` on `parent_type`, or `None` when either is undefined or the
    /// parent has no fields.
    pub(crate) fn lookup_field(
        &self,
        parent_type: &Name,
        field_name: &Name,
    ) -> Option<&'schema ast::FieldDefinition> {
        match self.schema.schema().types.get(parent_type)? {
            ExtendedType::Object(ty) => ty.fields.get(field_name).map(|field| &***field),
            ExtendedType::Interface(ty) => ty.fields.get(field_name).map(|field| &***field),
            _ => None,
        }
    }

    /// Does this output type bottom out in an object, interface, or union?
    pub(crate) fn is_composite(&self, ty: &ast::Type) -> bool {
        matches!(
            self.schema.schema().types.get(ty.inner_named_type()),
            Some(ExtendedType::Object(_) | ExtendedType::Interface(_) | ExtendedType::Union(_))
        )
    }
}
