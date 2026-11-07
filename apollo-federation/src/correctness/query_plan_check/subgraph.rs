//! The federation metadata a subgraph schema carries: `@key`, `@requires`, `@external`,
//! `@provides`, `@context` and `@fromContext`.
//!
//! The model keeps these beside the schema (`Subgraph.keys`, `Subgraph.requires`,
//! `Subgraph.resolves`, `Subgraph.provides`, `Subgraph.contextArguments`, `Subgraph.contextTypes`)
//! because its `Schema` does not model directives. Here they are read from the subgraph schema
//! itself, and what the subgraph can resolve from the metadata the query graph is built from.
//!
//! Field sets are written as strings in the directives. A `@key` or `@requires` field set is parsed
//! against the **supergraph** schema, not the subgraph's: it names the requirement the plan must
//! already have satisfied, and that requirement is compared against selections written in
//! supergraph terms. A `@provides` field set is parsed against the subgraph's own schema: what it
//! grants is read against a fetch's operation, which is written in the subgraph's terms. So is a
//! `@fromContext` selection, at the first type declaring its context, where composition has
//! validated it; it is read by field name and type condition, which is all a reading of it needs.
//!
//! What a field's result can be is read here too: its possible runtime types, each a subgraph
//! object with the supergraph object it stands for through the declaration correspondence, an
//! `@interfaceObject` standing for every object of the supergraph interface of its name
//! ([`Subgraph::field_runtime_types`]).

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast;
use apollo_compiler::executable::FieldSet;
use apollo_compiler::executable::Selection;
use apollo_compiler::schema::Directive;
use apollo_compiler::schema::DirectiveDefinition;
use apollo_compiler::schema::ExtendedType;

use super::super::query_compare::schema_view::SchemaView;
use super::ComparisonError;
use super::region::Work;
use super::selections::possible_types;
use crate::error::FederationError;
use crate::internal_error;
use crate::link::federation_spec_definition::FederationSpecDefinition;
use crate::link::federation_spec_definition::get_federation_spec_definition_from_subgraph;
use crate::schema::ValidFederationSchema;
use crate::schema::field_set::parse_field_value_without_validation;
use crate::schema::position::FieldDefinitionPosition;
use crate::schema::position::ObjectFieldDefinitionPosition;
use crate::schema::subgraph_metadata::SubgraphMetadata;
use crate::schema::validators::from_context::parse_context;

/// One `@key` application on an entity type in a subgraph.
pub(super) struct KeyDirective {
    /// The key's field set, as written.
    pub(super) fields: String,
    /// Whether the subgraph will resolve the entity from this key. An entity fetch may only rely
    /// on a resolvable key.
    pub(super) resolvable: bool,
}

/// One `@fromContext` argument of a field: the argument's name, the context it is taken from and
/// the declaration's selection, the two halves `parse_context` splits the directive's `field` into.
/// The model's `ContextArgument`.
#[derive(Clone, PartialEq)]
pub(super) struct ContextArgument {
    pub(super) name: Name,
    pub(super) context: Name,
    /// Parsed at the first type declaring the context in the subgraph. Composition rejects an
    /// alias in it, so its fields are read by name.
    pub(super) selection: Vec<Selection>,
}

/// A subgraph's federation metadata.
pub(super) struct Subgraph<'a> {
    schema: &'a ValidFederationSchema,
    federation: &'a FederationSpecDefinition,
    /// The reading of `@external` the query graph is built from.
    metadata: &'a SubgraphMetadata,
}

impl<'a> Subgraph<'a> {
    pub(super) fn new(schema: &'a ValidFederationSchema) -> Result<Self, ComparisonError> {
        let federation = get_federation_spec_definition_from_subgraph(schema).map_err(|e| {
            ComparisonError::new(format!("subgraph has no federation spec definition: {e}"))
        })?;
        let metadata = schema.subgraph_metadata().ok_or_else(|| {
            ComparisonError::new("subgraph schema carries no federation metadata".to_string())
        })?;
        Ok(Subgraph {
            schema,
            federation,
            metadata,
        })
    }

    /// The subgraph's own schema, which a fetch's operation is written against.
    pub(super) fn schema(&self) -> &'a ValidFederationSchema {
        self.schema
    }

    /// A field's definition on an object or interface type, by type and field name.
    pub(super) fn field(
        &self,
        type_name: &Name,
        field_name: &Name,
    ) -> Option<&'a Node<ast::FieldDefinition>> {
        match self.schema.schema().types.get(type_name) {
            Some(ExtendedType::Object(ty)) => ty.fields.get(field_name),
            Some(ExtendedType::Interface(ty)) => ty.fields.get(field_name),
            _ => None,
        }
    }

    /// Is an object one of a type's objects in this subgraph: the type itself, or a member or an
    /// implementation of it? The model's `typeIncludesObjectBool` in the subgraph's schema, which
    /// is where a fetch's own fragments are read.
    pub(super) fn type_includes_object(&self, type_name: &Name, object_type: &Name) -> bool {
        type_name == object_type || possible_types(self.schema, type_name).contains(object_type)
    }

    /// The supergraph objects a subgraph object stands for, through the declaration
    /// correspondence: the model's `Subgraph.objectCases`. An ordinary object stands for the
    /// supergraph object of its name; an `@interfaceObject` for every object of the supergraph
    /// interface of its name, without choosing one, each copied. `None`, an unjustified
    /// correspondence, when there is no such subgraph object, no supergraph object or interface of
    /// its name, or an interface object whose interface has no object.
    fn supergraph_objects(
        &self,
        work: &Work,
        supergraph: &SchemaView<'_>,
        subgraph_type: &Name,
    ) -> Option<Vec<Name>> {
        let Some(ExtendedType::Object(_)) = self.schema.schema().types.get(subgraph_type) else {
            return None;
        };
        let declared = supergraph.schema().schema().types.get(subgraph_type);
        if self.metadata.is_interface_object_type(subgraph_type) {
            let Some(ExtendedType::Interface(_)) = declared else {
                return None;
            };
            let objects = supergraph.possible_types(subgraph_type).to_vec();
            work.correspondence_names.add(objects.len() as u64);
            (!objects.is_empty()).then_some(objects)
        } else {
            matches!(declared, Some(ExtendedType::Object(_))).then(|| {
                work.correspondence_names.add(1);
                vec![subgraph_type.clone()]
            })
        }
    }

    /// Does the subgraph object `subgraph_type` stand for the supergraph object `object` through the
    /// declaration correspondence: is `object` one of [`Self::supergraph_objects`]? Decided without
    /// enumerating them, with the same declarations checked: an ordinary object stands for the
    /// supergraph object of its name, and an `@interfaceObject` for a supergraph object that
    /// declares it implements the supergraph interface of the interface object's name, which is
    /// what makes an object one of that interface's objects. A constant number of map lookups and
    /// one name read: `object`'s compared with `subgraph_type`, or the interface looked up among
    /// `object`'s.
    pub(super) fn stands_for(
        &self,
        work: &Work,
        supergraph: &SchemaView<'_>,
        subgraph_type: &Name,
        object: &Name,
    ) -> bool {
        let Some(ExtendedType::Object(_)) = self.schema.schema().types.get(subgraph_type) else {
            return false;
        };
        let types = &supergraph.schema().schema().types;
        work.correspondence_names.add(1);
        if self.metadata.is_interface_object_type(subgraph_type) {
            matches!(types.get(subgraph_type), Some(ExtendedType::Interface(_)))
                && matches!(
                    types.get(object),
                    Some(ExtendedType::Object(declared))
                        if declared.implements_interfaces.contains(subgraph_type)
                )
        } else {
            subgraph_type == object
                && matches!(types.get(subgraph_type), Some(ExtendedType::Object(_)))
        }
    }

    /// The possible runtime types of a value of a subgraph type, each the subgraph object and the
    /// supergraph object it stands for: none for a leaf, and for a composite type each of its
    /// objects in the subgraph with each supergraph object it stands for. `None` when one of those
    /// has an unjustified correspondence. The model's `Subgraph.typeCases`.
    fn type_runtime_types(
        &self,
        work: &Work,
        supergraph: &SchemaView<'_>,
        subgraph_type: &Name,
    ) -> Option<Vec<(Name, Name)>> {
        match self.schema.schema().types.get(subgraph_type)? {
            ExtendedType::Scalar(_) | ExtendedType::Enum(_) => Some(Vec::new()),
            ExtendedType::Object(_) | ExtendedType::Interface(_) | ExtendedType::Union(_) => {
                let mut runtime_types = Vec::new();
                for object in possible_types(self.schema, subgraph_type) {
                    for stands_for in self.supergraph_objects(work, supergraph, &object)? {
                        runtime_types.push((object.clone(), stands_for));
                    }
                }
                Some(runtime_types)
            }
            ExtendedType::InputObject(_) => None,
        }
    }

    /// The possible runtime types of a field's result on a subgraph object type, whatever an
    /// operation selects of it: the model's `Subgraph.fieldCases`. `Some([])` for a scalar leaf,
    /// and for an abstract result with no possible object result types in the subgraph, which
    /// still owes the field; `None`, never read as either, for a missing declaration, of the parent
    /// or the field, or an unjustified correspondence.
    pub(super) fn field_runtime_types(
        &self,
        work: &Work,
        supergraph: &SchemaView<'_>,
        subgraph_parent: &Name,
        field_name: &Name,
    ) -> Option<Vec<(Name, Name)>> {
        let Some(ExtendedType::Object(parent)) = self.schema.schema().types.get(subgraph_parent)
        else {
            return None;
        };
        let field = parent.fields.get(field_name)?;
        self.type_runtime_types(work, supergraph, field.ty.inner_named_type())
    }

    /// Whether the subgraph can resolve a field of an object type outright, outside any
    /// `@provides` window: the model's `Subgraph.resolves`.
    ///
    /// It can unless the metadata reads the field as external: marked `@external`, or a field of a
    /// type marked `@external`. A field marked `@external` that a key of a type extension names is
    /// a *fake* external, which the metadata reads as one the subgraph resolves.
    pub(super) fn resolves(&self, object_type: &Name, field_name: &Name) -> bool {
        let position = FieldDefinitionPosition::Object(ObjectFieldDefinitionPosition {
            type_name: object_type.clone(),
            field_name: field_name.clone(),
        });
        !self.metadata.is_field_external(&position)
    }

    /// The subgraph a field's progressive override takes it from: the `from` of an `@override`
    /// carrying a `label`, by parent type and field name. Extraction keeps both declarations of a
    /// progressive override, and the query graph resolves the field through each under opposite
    /// conditions on the label, so neither is a provider interchangeable with the other.
    pub(super) fn progressively_overrides(
        &self,
        type_name: &Name,
        field_name: &Name,
    ) -> Result<Option<String>, ComparisonError> {
        self.field_directive(
            self.federation.override_directive_definition(self.schema),
            type_name,
            field_name,
            |field, directive| {
                let Some(application) = field.directives.get(directive) else {
                    return Ok(None);
                };
                let arguments = self.federation.override_directive_arguments(application)?;
                Ok(arguments.label.map(|_| arguments.from.to_string()))
            },
        )
        .map(Option::flatten)
    }

    /// The `@key` applications on a type. A type may carry several.
    pub(super) fn keys(&self, type_name: &Name) -> Result<Vec<KeyDirective>, ComparisonError> {
        let Ok(definition) = self.federation.key_directive_definition(self.schema) else {
            return Ok(Vec::new());
        };
        let Some(position) = self.schema.schema().types.get(type_name) else {
            return Ok(Vec::new());
        };
        position
            .directives()
            .get_all(&definition.name)
            .map(|directive| {
                let arguments =
                    self.federation
                        .key_directive_arguments(directive)
                        .map_err(|e| {
                            ComparisonError::new(format!("malformed @key on {type_name}: {e}"))
                        })?;
                Ok(KeyDirective {
                    fields: arguments.fields.to_string(),
                    resolvable: arguments.resolvable,
                })
            })
            .collect()
    }

    /// The `@requires` field set of a field, by parent type and field name.
    pub(super) fn requires(
        &self,
        type_name: &Name,
        field_name: &Name,
    ) -> Result<Option<String>, ComparisonError> {
        self.field_directive(
            self.federation.requires_directive_definition(self.schema),
            type_name,
            field_name,
            |field, directive| {
                field
                    .directives
                    .get(directive)
                    .map(|application| {
                        self.federation
                            .requires_directive_arguments(application)
                            .map(|arguments| arguments.fields.to_string())
                    })
                    .transpose()
            },
        )
        .map(Option::flatten)
    }

    /// The `@provides` field set of a field, by parent type and field name, parsed against this
    /// subgraph's schema at the field's own type: the model's `Subgraph.provides`.
    pub(super) fn provides(
        &self,
        type_name: &Name,
        field_name: &Name,
    ) -> Result<Option<Vec<Selection>>, ComparisonError> {
        let fields = self
            .field_directive(
                self.federation.provides_directive_definition(self.schema),
                type_name,
                field_name,
                |field, directive| {
                    field
                        .directives
                        .get(directive)
                        .map(|application| {
                            self.federation
                                .provides_directive_arguments(application)
                                .map(|arguments| arguments.fields.to_string())
                        })
                        .transpose()
                },
            )?
            .flatten();
        let (Some(fields), Some(field)) = (fields, self.field(type_name, field_name)) else {
            return Ok(None);
        };
        parse_field_set(self.schema, field.ty.inner_named_type(), &fields).map(Some)
    }

    /// The `@fromContext` arguments of a field, by parent type and field name: the model's
    /// `Subgraph.contextArguments`.
    pub(super) fn context_arguments(
        &self,
        type_name: &Name,
        field_name: &Name,
    ) -> Result<Vec<ContextArgument>, ComparisonError> {
        self.field_directive(
            self.federation
                .from_context_directive_definition(self.schema),
            type_name,
            field_name,
            |field, directive| {
                field
                    .arguments
                    .iter()
                    .filter_map(|argument| {
                        let application = argument.directives.get(directive)?;
                        Some(self.context_argument(&argument.name, application))
                    })
                    .collect()
            },
        )
        .map(Option::unwrap_or_default)
    }

    /// One `@fromContext` application, its `field` split into the context and the selection, and
    /// the selection parsed at the first type declaring the context.
    fn context_argument(
        &self,
        name: &Name,
        application: &Node<Directive>,
    ) -> Result<ContextArgument, FederationError> {
        let field = self
            .federation
            .from_context_directive_arguments(application)?
            .field;
        let (Some(context), Some(selection)) = parse_context(field) else {
            return Err(internal_error!("`{field}` names no context and selection"));
        };
        let context = Name::new(&context)?;
        let Some(declaring_type) = self.declaring_types(&context)?.into_iter().next() else {
            return Err(internal_error!("no type declares context `{context}`"));
        };
        let selection =
            parse_field_value_without_validation(self.schema, declaring_type, &selection)?
                .selection_set
                .selections;
        Ok(ContextArgument {
            name: name.clone(),
            context,
            selection,
        })
    }

    /// The types declaring a context, `@context(name:)` on an object, an interface or a union, in
    /// the order the schema holds them: the model's `Subgraph.contextTypes`. Extraction writes a
    /// context into the subgraph under the name the subgraph gave it.
    pub(super) fn context_types(&self, context: &Name) -> Result<Vec<Name>, ComparisonError> {
        self.declaring_types(context).map_err(|e| {
            ComparisonError::new(format!("malformed @context declaring `{context}`: {e}"))
        })
    }

    fn declaring_types(&self, context: &Name) -> Result<Vec<Name>, FederationError> {
        let Ok(definition) = self.federation.context_directive_definition(self.schema) else {
            return Ok(Vec::new());
        };
        let mut types = Vec::new();
        for (type_name, ty) in &self.schema.schema().types {
            for application in ty.directives().get_all(&definition.name) {
                if self
                    .federation
                    .context_directive_arguments(application)?
                    .name
                    == context.as_str()
                {
                    types.push(type_name.clone());
                }
            }
        }
        Ok(types)
    }

    /// Reads a federation directive's applications on a field, by parent type and field name:
    /// `read` is handed the field and the name the directive has in this subgraph, and finds the
    /// applications on the field or on its arguments. `None` when the subgraph does not define the
    /// directive or does not declare the field.
    fn field_directive<T>(
        &self,
        definition: Result<&Node<DirectiveDefinition>, FederationError>,
        type_name: &Name,
        field_name: &Name,
        read: impl FnOnce(&'a Node<ast::FieldDefinition>, &Name) -> Result<T, FederationError>,
    ) -> Result<Option<T>, ComparisonError> {
        let Ok(definition) = definition else {
            return Ok(None);
        };
        let Some(field) = self.field(type_name, field_name) else {
            return Ok(None);
        };
        read(field, &definition.name).map(Some).map_err(|e| {
            ComparisonError::new(format!(
                "malformed @{} on {type_name}.{field_name}: {e}",
                definition.name
            ))
        })
    }
}

/// Parses a field set written in a `@key`, `@requires` or `@provides` argument, against the schema
/// it is read in: the supergraph for a `@key` or `@requires`, the subgraph for a `@provides`.
pub(super) fn parse_field_set(
    schema: &ValidFederationSchema,
    parent_type: &Name,
    field_set: &str,
) -> Result<Vec<Selection>, ComparisonError> {
    FieldSet::parse_and_validate(
        schema.schema(),
        parent_type.clone(),
        field_set,
        "field_set.graphql",
    )
    .map(|parsed| parsed.into_inner().selection_set.selections)
    .map_err(|e| {
        ComparisonError::new(format!(
            "field set `{field_set}` is not valid on {parent_type}:\n{e}"
        ))
    })
}
