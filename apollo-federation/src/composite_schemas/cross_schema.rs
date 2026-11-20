//! Validation of `@is` and `@require` selection maps against other source schemas
//! (`IS_INVALID_FIELDS`, `REQUIRE_INVALID_FIELDS`).
//!
//! The specification places these rules after merging, validating against "the union of other
//! schemas except all fields marked as `@internal`". The union is taken directly over the
//! validated subgraphs here, so the rules do not depend on merge output (the specification permits
//! reordering checks). Two choices differ from the letter of the specification:
//! - An `@is` map is validated against every subgraph, *including* the one declaring it. The
//!   specification's own valid `IS_INVALID_FIELDS` example is a single schema whose `@is` maps to
//!   a field of that same schema, which "the union of other schemas" would reject.
//! - A `@require` map is validated against the other subgraphs without their `@external` fields,
//!   which those subgraphs cannot resolve.
//!
//! Two consequences of merging are taken into account, since the union of unmerged subgraphs would
//! miss them:
//! - Type relationships (which types are composite, and the possible types of an abstract type)
//!   come from every subgraph, including the declaring one: a type condition `<User>` on an
//!   interface `Account` applies if any subgraph declares `User implements Account`.
//! - A field an `@interfaceObject` stand-in declares is a field of every implementation of that
//!   interface, so it can be selected on an implementation type the stand-in's subgraph does not
//!   declare.

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::Schema;
use apollo_compiler::collections::IndexSet;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::schema::FieldDefinition;

use super::CompositeNames;
use super::validation::ArgumentCoordinate;
use super::validation::fields_of;
use super::validation::parse_field_selection_map_argument;
use crate::error::CompositionError;
use crate::error::SingleFederationError;
use crate::link::federation_spec_definition::FEDERATION_EXTERNAL_DIRECTIVE_NAME_IN_SPEC;
use crate::link::federation_spec_definition::FEDERATION_FROM_ARGUMENT_NAME;
use crate::link::federation_spec_definition::FEDERATION_INTERFACEOBJECT_DIRECTIVE_NAME_IN_SPEC;
use crate::link::federation_spec_definition::FEDERATION_INTERNAL_DIRECTIVE_NAME_IN_SPEC;
use crate::link::federation_spec_definition::FEDERATION_OVERRIDE_DIRECTIVE_NAME_IN_SPEC;
use crate::link::federation_spec_definition::FEDERATION_REQUIRE_DIRECTIVE_NAME_IN_SPEC;
use crate::link::spec_definition::SpecDefinition;
use crate::schema::field_selection_map::SelectedObjectField;
use crate::schema::field_selection_map::SelectedValue;
use crate::schema::field_selection_map::SelectedValueEntry;
use crate::schema::field_selection_map::validate::OutputSchema;
use crate::schema::field_selection_map::validate::field_in_schema;
use crate::schema::field_selection_map::validate::is_composite_in_schema;
use crate::schema::field_selection_map::validate::is_leaf_in_schema;
use crate::schema::field_selection_map::validate::possible_types;
use crate::schema::field_selection_map::validate::validate;
use crate::subgraph::typestate::Subgraph;
use crate::subgraph::typestate::Validated;

/// One subgraph's contribution to a cross-schema output side.
struct Member<'a> {
    name: &'a str,
    schema: &'a Schema,
    internal: Option<Name>,
    external: Option<Name>,
    override_: Option<Name>,
    require: Option<Name>,
    interface_object: Option<Name>,
}

impl Member<'_> {
    /// Whether `directive`, if this subgraph defines it, is applied to the type `type_name` or to
    /// `field`.
    fn marks(
        &self,
        directive: &Option<Name>,
        type_name: &Name,
        field: Option<&FieldDefinition>,
    ) -> bool {
        let Some(directive) = directive else {
            return false;
        };
        field.is_some_and(|field| field.directives.has(directive))
            || self
                .schema
                .types
                .get(type_name)
                .is_some_and(|ty| ty.directives().has(directive))
    }

    /// Whether this subgraph declares `type_name.field_name` itself, in the sense of
    /// `REQUIRE_INVALID_FIELDS`: a field that is not `@external` or `@internal` (directly or
    /// through its type) and that no other subgraph overrides from this one.
    fn declares(&self, type_name: &Name, field_name: &Name, members: &[Member<'_>]) -> bool {
        let Some(field) = field_in_schema(self.schema, type_name, field_name) else {
            return false;
        };
        if self.marks(&self.external, type_name, Some(field))
            || self.marks(&self.internal, type_name, Some(field))
        {
            return false;
        }
        let overridden = members.iter().any(|other| {
            let Some(override_) = &other.override_ else {
                return false;
            };
            other.name != self.name
                && field_in_schema(other.schema, type_name, field_name)
                    .and_then(|field| field.directives.get(override_))
                    .and_then(|directive| {
                        directive.specified_argument_by_name(&FEDERATION_FROM_ARGUMENT_NAME)
                    })
                    .and_then(|from| from.as_str())
                    .is_some_and(|from| from == self.name)
        });
        !overridden
    }
}

/// The fields a selection map selects directly on its root type: the first segment of each root
/// path and the fields of each root selected object. Paths with a root type condition (`<T>.f`)
/// select from another type and are not included.
fn root_fields(value: &SelectedValue) -> IndexSet<Name> {
    fn collect(value: &SelectedValue, fields: &mut IndexSet<Name>) {
        for entry in &value.alternatives {
            let path = match entry {
                SelectedValueEntry::Path(path)
                | SelectedValueEntry::PathObject(path, _)
                | SelectedValueEntry::PathList(path, _) => path,
                SelectedValueEntry::Object(object) => {
                    for field in &object.fields {
                        match field {
                            SelectedObjectField::Shorthand(name, _) => {
                                fields.insert(name.clone());
                            }
                            SelectedObjectField::Labeled(_, value) => collect(value, fields),
                        }
                    }
                    continue;
                }
            };
            if path.type_condition.is_none()
                && let Some(first) = path.segments.first()
            {
                fields.insert(first.field.clone());
            }
        }
    }
    let mut fields = IndexSet::default();
    collect(value, &mut fields);
    fields
}

/// The union of several subgraph schemas, without `@internal` elements, and optionally without
/// one subgraph and without `@external` fields (a requirement must be resolvable by another
/// subgraph). The left-out subgraph only hides its fields: type relationships come from every
/// subgraph, since they all contribute to the merged schema.
struct SubgraphsOutput<'a> {
    members: &'a [Member<'a>],
    /// The subgraph left out, by name.
    skip: Option<&'a str>,
    exclude_external: bool,
}

impl<'a> SubgraphsOutput<'a> {
    fn members(&self) -> impl Iterator<Item = &'a Member<'a>> {
        let skip = self.skip;
        self.members
            .iter()
            .filter(move |member| Some(member.name) != skip)
    }

    /// `type_name.field_name` as declared directly by one of the selectable subgraphs.
    fn declared_field(
        &self,
        type_name: &Name,
        field_name: &Name,
    ) -> Option<(&'a Node<FieldDefinition>, &'a Schema, Option<&'a Name>)> {
        self.members().find_map(|member| {
            let field = field_in_schema(member.schema, type_name, field_name)?;
            let hidden = member.marks(&member.internal, type_name, Some(field))
                || (self.exclude_external
                    && member.marks(&member.external, type_name, Some(field)));
            (!hidden).then_some((field, member.schema, member.require.as_ref()))
        })
    }

    /// `type_name.field_name` as declared by an `@interfaceObject` stand-in, in one of the
    /// selectable subgraphs, for an interface that `type_name` implements in any subgraph.
    fn interface_object_field(
        &self,
        type_name: &Name,
        field_name: &Name,
    ) -> Option<(&'a Node<FieldDefinition>, &'a Schema, Option<&'a Name>)> {
        let interfaces: IndexSet<&Name> = self
            .members
            .iter()
            .filter_map(|member| match member.schema.types.get(type_name)? {
                ExtendedType::Object(object) => Some(&object.implements_interfaces),
                ExtendedType::Interface(interface) => Some(&interface.implements_interfaces),
                _ => None,
            })
            .flatten()
            .map(|interface| &**interface)
            .collect();
        interfaces.into_iter().find_map(|interface| {
            self.members().find_map(|member| {
                let is_stand_in = matches!(
                    member.schema.types.get(interface),
                    Some(ExtendedType::Object(_))
                ) && member.marks(&member.interface_object, interface, None);
                if !is_stand_in {
                    return None;
                }
                let field = field_in_schema(member.schema, interface, field_name)?;
                let hidden = member.marks(&member.internal, interface, Some(field))
                    || (self.exclude_external
                        && member.marks(&member.external, interface, Some(field)));
                (!hidden).then_some((field, member.schema, member.require.as_ref()))
            })
        })
    }
}

impl OutputSchema for SubgraphsOutput<'_> {
    fn field(
        &self,
        type_name: &Name,
        field_name: &Name,
    ) -> Option<(&Node<FieldDefinition>, &Schema, Option<&Name>)> {
        self.declared_field(type_name, field_name)
            .or_else(|| self.interface_object_field(type_name, field_name))
    }

    fn is_composite(&self, type_name: &Name) -> bool {
        self.members.iter().any(|m| {
            !m.marks(&m.internal, type_name, None) && is_composite_in_schema(m.schema, type_name)
        })
    }

    fn is_leaf(&self, type_name: &Name) -> bool {
        self.members()
            .any(|m| is_leaf_in_schema(m.schema, type_name))
    }

    fn possible_types(&self, type_name: &Name) -> IndexSet<Name> {
        // A stand-in is an object type in its subgraph, but not a possible type of the interface.
        self.members
            .iter()
            .filter(|m| {
                !m.marks(&m.internal, type_name, None)
                    && !m.marks(&m.interface_object, type_name, None)
            })
            .flat_map(|m| possible_types(m.schema, type_name))
            .collect()
    }
}

fn member<'a>(subgraph: &'a Subgraph<Validated>) -> Member<'a> {
    let schema = subgraph.validated_schema();
    let spec = subgraph.metadata().federation_spec_definition();
    let name = |n: &Name| {
        spec.directive_name_in_schema(subgraph.schema(), n)
            .filter(|n| schema.schema().directive_definitions.contains_key(n))
    };
    Member {
        name: &subgraph.name,
        schema: schema.schema(),
        internal: name(&FEDERATION_INTERNAL_DIRECTIVE_NAME_IN_SPEC),
        external: name(&FEDERATION_EXTERNAL_DIRECTIVE_NAME_IN_SPEC),
        override_: name(&FEDERATION_OVERRIDE_DIRECTIVE_NAME_IN_SPEC),
        require: name(&FEDERATION_REQUIRE_DIRECTIVE_NAME_IN_SPEC),
        interface_object: name(&FEDERATION_INTERFACEOBJECT_DIRECTIVE_NAME_IN_SPEC),
    }
}

/// Validate every `@is` and `@require` selection map of every subgraph that applies the GraphQL
/// Federation directives against the other subgraphs.
pub(crate) fn validate_selection_maps(subgraphs: &[Subgraph<Validated>]) -> Vec<CompositionError> {
    let members: Vec<Member<'_>> = subgraphs.iter().map(member).collect();
    let is_output = SubgraphsOutput {
        members: &members,
        skip: None,
        exclude_external: false,
    };
    let mut errors = Vec::new();
    for (subgraph, own) in subgraphs.iter().zip(&members) {
        if !subgraph.metadata().uses_composite_directives() {
            continue;
        }
        let Some(names) = CompositeNames::new(
            subgraph.schema(),
            subgraph.metadata().federation_spec_definition(),
        ) else {
            continue;
        };
        let schema = own.schema;
        let require_output = SubgraphsOutput {
            members: &members,
            skip: Some(own.name),
            exclude_external: true,
        };
        let mut report = |error: SingleFederationError| {
            errors.push(CompositionError::SubgraphError {
                subgraph: subgraph.name.clone(),
                error,
                locations: Vec::new(),
            })
        };
        for (type_name, ty) in &schema.types {
            let Some(fields) = fields_of(ty) else {
                continue;
            };
            for field in fields.values() {
                for argument in &field.arguments {
                    let coordinate = ArgumentCoordinate {
                        type_name,
                        field,
                        argument,
                    };
                    if let Some(is) = argument.directives.get(&names.is) {
                        let Ok(map) = parse_field_selection_map_argument(
                            is,
                            "is",
                            &coordinate,
                            |message| SingleFederationError::IsInvalidFieldType { message },
                            |message| SingleFederationError::IsInvalidSyntax { message },
                        ) else {
                            continue;
                        };
                        let root = field.ty.inner_named_type();
                        for error in validate(&map, &is_output, root, schema, &argument.ty) {
                            report(SingleFederationError::IsInvalidFields {
                                message: format!(
                                    "The @is directive on \"{coordinate}\" has an invalid field \
                                     selection map \"{map}\": {}.",
                                    error.message
                                ),
                            });
                        }
                    }
                    if let Some(require) = argument.directives.get(&names.require) {
                        let Ok(map) = parse_field_selection_map_argument(
                            require,
                            "require",
                            &coordinate,
                            |message| SingleFederationError::RequireInvalidFieldType { message },
                            |message| SingleFederationError::RequireInvalidSyntax { message },
                        ) else {
                            continue;
                        };
                        // A field this subgraph declares itself is reported as such, rather than
                        // as one the other subgraphs may not resolve.
                        let locally_declared: Vec<Name> = root_fields(&map)
                            .into_iter()
                            .filter(|field_name| own.declares(type_name, field_name, &members))
                            .collect();
                        for field_name in &locally_declared {
                            report(SingleFederationError::RequireInvalidFields {
                                message: format!(
                                    "The @require directive on \"{coordinate}\" selects \
                                     \"{field_name}\", which \"{type_name}\" already declares in \
                                     this subgraph: require only fields resolved by another \
                                     subgraph."
                                ),
                            });
                        }
                        if !locally_declared.is_empty() {
                            continue;
                        }
                        for error in
                            validate(&map, &require_output, type_name, schema, &argument.ty)
                        {
                            report(SingleFederationError::RequireInvalidFields {
                                message: format!(
                                    "The @require directive on \"{coordinate}\" has an invalid field \
                                     selection map \"{map}\": {} (a requirement must select fields \
                                     resolvable by another subgraph).",
                                    error.message
                                ),
                            });
                        }
                    }
                }
            }
        }
    }
    errors
}
