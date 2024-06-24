mod spec;

use std::collections::HashSet;
use std::fmt::Debug;
use std::fmt::Formatter;
use std::iter;
use std::sync::Arc;

use apollo_compiler::ast::Argument;
use apollo_compiler::ast::Directive;
use apollo_compiler::ast::DirectiveDefinition;
use apollo_compiler::ast::DirectiveLocation;
use apollo_compiler::ast::EnumValueDefinition;
use apollo_compiler::ast::FieldDefinition;
use apollo_compiler::ast::NamedType;
use apollo_compiler::ast::Value;
use apollo_compiler::name;
use apollo_compiler::schema::Component;
use apollo_compiler::schema::EnumType;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::schema::InputObjectType;
use apollo_compiler::schema::InputValueDefinition;
use apollo_compiler::schema::InterfaceType;
use apollo_compiler::schema::Name;
use apollo_compiler::schema::ObjectType;
use apollo_compiler::schema::UnionType;
use apollo_compiler::validation::Valid;
use apollo_compiler::Node;
use apollo_compiler::NodeStr;
use apollo_compiler::Schema;
use indexmap::map::Entry::Occupied;
use indexmap::map::Entry::Vacant;
use indexmap::map::Iter;
use indexmap::IndexMap;
use indexmap::IndexSet;
use itertools::Itertools;
use spec::add_core_feature_join;
use spec::add_core_feature_link;
use spec::join_field_applied_directive;

use crate::error::FederationError;
use crate::link::federation_spec_definition::FEDERATION_EXTERNAL_DIRECTIVE_NAME_IN_SPEC;
use crate::link::federation_spec_definition::FEDERATION_INTERFACEOBJECT_DIRECTIVE_NAME_IN_SPEC;
use crate::link::federation_spec_definition::FEDERATION_KEY_DIRECTIVE_NAME_IN_SPEC;
use crate::link::federation_spec_definition::FEDERATION_PROVIDES_DIRECTIVE_NAME_IN_SPEC;
use crate::link::federation_spec_definition::FEDERATION_REQUIRES_DIRECTIVE_NAME_IN_SPEC;
use crate::link::spec::Identity;
use crate::link::LinksMetadata;
use crate::schema::ValidFederationSchema;
use crate::subgraph::ValidSubgraph;
use crate::ValidFederationSubgraph;
use crate::ValidFederationSubgraphs;

type MergeWarning = String;
type MergeError = String;

struct Merger {
    errors: Vec<MergeError>,
    composition_hints: Vec<MergeWarning>,
}

pub struct MergeSuccess {
    pub schema: Valid<Schema>,
    pub composition_hints: Vec<MergeWarning>,
}

impl From<FederationError> for MergeFailure {
    fn from(err: FederationError) -> Self {
        // TODO: Consider an easier transition / interop between MergeFailure and FederationError
        // TODO: This is most certainly not the right error kind. MergeFailure's
        // errors need to be in an enum that could be matched on rather than a
        // str.
        MergeFailure {
            schema: None,
            errors: vec![err.to_string()],
            composition_hints: vec![],
        }
    }
}

pub struct MergeFailure {
    pub schema: Option<Schema>,
    pub errors: Vec<MergeError>,
    pub composition_hints: Vec<MergeWarning>,
}

impl Debug for MergeFailure {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result<(), std::fmt::Error> {
        f.debug_struct("MergeFailure")
            .field("errors", &self.errors)
            .field("composition_hints", &self.composition_hints)
            .finish()
    }
}

pub fn merge_subgraphs(subgraphs: Vec<&ValidSubgraph>) -> Result<MergeSuccess, MergeFailure> {
    let mut merger = Merger::new();
    let mut federation_subgraphs = ValidFederationSubgraphs::new();
    for subgraph in subgraphs {
        federation_subgraphs.add(ValidFederationSubgraph {
            name: subgraph.name.clone(),
            url: subgraph.url.clone(),
            schema: ValidFederationSchema::new(subgraph.schema.clone())?,
        })?;
    }
    merger.merge(federation_subgraphs)
}

pub fn merge_federation_subgraphs(
    subgraphs: ValidFederationSubgraphs,
) -> Result<MergeSuccess, MergeFailure> {
    let mut merger = Merger::new();
    merger.merge(subgraphs)
}

impl Merger {
    fn new() -> Self {
        Merger {
            composition_hints: Vec::new(),
            errors: Vec::new(),
        }
    }

    fn merge(&mut self, subgraphs: ValidFederationSubgraphs) -> Result<MergeSuccess, MergeFailure> {
        let mut subgraphs = subgraphs
            .into_iter()
            .map(|(_, subgraph)| subgraph)
            .collect_vec();
        subgraphs.sort_by(|s1, s2| s1.name.cmp(&s2.name));
        let mut subgraphs_and_enum_values: Vec<(&ValidFederationSubgraph, Name)> = Vec::new();
        for subgraph in &subgraphs {
            // TODO: Implement JS codebase's name transform (which always generates a valid GraphQL
            // name and avoids collisions).
            if let Ok(subgraph_name) = Name::new(&subgraph.name.to_uppercase()) {
                subgraphs_and_enum_values.push((subgraph, subgraph_name));
            } else {
                self.errors.push(String::from(
                    "Subgraph name couldn't be transformed into valid GraphQL name",
                ));
            }
        }
        if !self.errors.is_empty() {
            return Err(MergeFailure {
                schema: None,
                composition_hints: self.composition_hints.to_owned(),
                errors: self.errors.to_owned(),
            });
        }

        let mut supergraph = Schema::new();
        // TODO handle @compose

        // add core features
        // TODO verify federation versions across subgraphs
        add_core_feature_link(&mut supergraph);
        add_core_feature_join(&mut supergraph, &subgraphs_and_enum_values);

        // create stubs
        for (subgraph, subgraph_name) in &subgraphs_and_enum_values {
            let sources = Arc::make_mut(&mut supergraph.sources);
            for (key, source) in subgraph.schema.schema().sources.iter() {
                sources.entry(*key).or_insert_with(|| source.clone());
            }

            self.merge_schema(&mut supergraph, subgraph);
            // TODO merge directives

            let metadata = subgraph.schema.metadata();

            for (type_name, ty) in &subgraph.schema.schema().types {
                if ty.is_built_in() || !is_mergeable_type(type_name) {
                    // skip built-ins and federation specific types
                    continue;
                }

                match ty {
                    ExtendedType::Enum(value) => self.merge_enum_type(
                        &mut supergraph.types,
                        subgraph_name.clone(),
                        type_name.clone(),
                        value,
                    ),
                    ExtendedType::InputObject(value) => self.merge_input_object_type(
                        &mut supergraph.types,
                        subgraph_name.clone(),
                        type_name.clone(),
                        value,
                    ),
                    ExtendedType::Interface(value) => self.merge_interface_type(
                        &mut supergraph.types,
                        &metadata,
                        subgraph_name.clone(),
                        type_name.clone(),
                        value,
                    ),
                    ExtendedType::Object(value) => self.merge_object_type(
                        &mut supergraph.types,
                        &metadata,
                        subgraph_name.clone(),
                        type_name.clone(),
                        value,
                    ),
                    ExtendedType::Union(value) => self.merge_union_type(
                        &mut supergraph.types,
                        subgraph_name.clone(),
                        type_name.clone(),
                        value,
                    ),
                    ExtendedType::Scalar(_value) => {
                        supergraph.types.insert(type_name.clone(), ty.clone());
                    }
                }
            }

            // merge executable directives
            for (_, directive) in subgraph.schema.schema().directive_definitions.iter() {
                if is_executable_directive(directive) {
                    merge_directive(&mut supergraph.directive_definitions, directive);
                }
            }
        }

        if self.errors.is_empty() {
            // TODO: validate here and extend `MergeFailure` to propagate validation errors
            let supergraph = Valid::assume_valid(supergraph);
            Ok(MergeSuccess {
                schema: supergraph,
                composition_hints: self.composition_hints.to_owned(),
            })
        } else {
            Err(MergeFailure {
                schema: Some(supergraph),
                composition_hints: self.composition_hints.to_owned(),
                errors: self.errors.to_owned(),
            })
        }
    }

    fn merge_descriptions<T: Eq + Clone>(&mut self, merged: &mut Option<T>, new: &Option<T>) {
        match (&mut *merged, new) {
            (_, None) => {}
            (None, Some(_)) => merged.clone_from(new),
            (Some(a), Some(b)) => {
                if a != b {
                    // TODO add info about type and from/to subgraph
                    self.composition_hints
                        .push(String::from("conflicting descriptions"));
                }
            }
        }
    }

    fn merge_schema(&mut self, supergraph_schema: &mut Schema, subgraph: &ValidFederationSubgraph) {
        let supergraph_def = &mut supergraph_schema.schema_definition.make_mut();
        let subgraph_def = &subgraph.schema.schema().schema_definition;
        self.merge_descriptions(&mut supergraph_def.description, &subgraph_def.description);

        if subgraph_def.query.is_some() {
            supergraph_def.query.clone_from(&subgraph_def.query);
            // TODO mismatch on query types
        }
        if subgraph_def.mutation.is_some() {
            supergraph_def.mutation.clone_from(&subgraph_def.mutation);
            // TODO mismatch on mutation types
        }
        if subgraph_def.subscription.is_some() {
            supergraph_def
                .subscription
                .clone_from(&subgraph_def.subscription);
            // TODO mismatch on subscription types
        }
    }

    fn merge_enum_type(
        &mut self,
        types: &mut IndexMap<NamedType, ExtendedType>,
        subgraph_name: Name,
        enum_name: NamedType,
        enum_type: &Node<EnumType>,
    ) {
        let existing_type = types
            .entry(enum_name.clone())
            .or_insert(copy_enum_type(enum_name, enum_type));
        if let ExtendedType::Enum(e) = existing_type {
            let join_type_directives =
                join_type_applied_directive(subgraph_name.clone(), iter::empty(), false);
            e.make_mut().directives.extend(join_type_directives);

            self.merge_descriptions(&mut e.make_mut().description, &enum_type.description);

            // TODO we need to merge those fields LAST so we know whether enum is used as input/output/both as different merge rules will apply
            // below logic only works for output enums
            for (enum_value_name, enum_value) in enum_type.values.iter() {
                let ev = e
                    .make_mut()
                    .values
                    .entry(enum_value_name.clone())
                    .or_insert(Component::new(EnumValueDefinition {
                        value: enum_value.value.clone(),
                        description: None,
                        directives: Default::default(),
                    }));
                self.merge_descriptions(&mut ev.make_mut().description, &enum_value.description);
                ev.make_mut().directives.push(Node::new(Directive {
                    name: name!("join__enumValue"),
                    arguments: vec![
                        (Node::new(Argument {
                            name: name!("graph"),
                            value: Node::new(Value::Enum(subgraph_name.clone())),
                        })),
                    ],
                }));
            }
        } else {
            // TODO - conflict
        }
    }

    fn merge_input_object_type(
        &mut self,
        types: &mut IndexMap<NamedType, ExtendedType>,
        subgraph_name: Name,
        input_object_name: NamedType,
        input_object: &Node<InputObjectType>,
    ) {
        let existing_type = types
            .entry(input_object_name.clone())
            .or_insert(copy_input_object_type(input_object_name, input_object));
        if let ExtendedType::InputObject(obj) = existing_type {
            let join_type_directives =
                join_type_applied_directive(subgraph_name, iter::empty(), false);
            let mutable_object = obj.make_mut();
            mutable_object.directives.extend(join_type_directives);

            for (field_name, _field) in input_object.fields.iter() {
                let existing_field = mutable_object.fields.entry(field_name.clone());
                match existing_field {
                    Vacant(_i) => {
                        // TODO warning - mismatch on input fields
                    }
                    Occupied(_i) => {
                        // merge_options(&i.get_mut().description, &field.description);
                        // TODO check description
                        // TODO check type
                        // TODO check default value
                        // TODO process directives
                    }
                }
            }
        } else {
            // TODO conflict on type
        }
    }

    fn merge_interface_type(
        &mut self,
        types: &mut IndexMap<NamedType, ExtendedType>,
        metadata: &Option<&LinksMetadata>,
        subgraph_name: Name,
        interface_name: NamedType,
        interface: &Node<InterfaceType>,
    ) {
        let federation_identity =
            metadata.and_then(|m| m.by_identity.get(&Identity::federation_identity()));

        let key_directive_name = federation_identity
            .map(|link| link.directive_name_in_schema(&FEDERATION_KEY_DIRECTIVE_NAME_IN_SPEC))
            .unwrap_or(FEDERATION_KEY_DIRECTIVE_NAME_IN_SPEC);

        let existing_type = types
            .entry(interface_name.clone())
            .or_insert(copy_interface_type(interface_name, interface));
        if let ExtendedType::Interface(intf) = existing_type {
            let key_directives = interface.directives.get_all(&key_directive_name);
            let join_type_directives =
                join_type_applied_directive(subgraph_name, key_directives, false);
            let mutable_intf = intf.make_mut();
            mutable_intf.directives.extend(join_type_directives);

            for (field_name, field) in interface.fields.iter() {
                let existing_field = mutable_intf.fields.entry(field_name.clone());
                match existing_field {
                    Vacant(i) => {
                        // TODO warning mismatch missing fields
                        i.insert(Component::new(FieldDefinition {
                            name: field.name.clone(),
                            description: field.description.clone(),
                            arguments: vec![],
                            ty: field.ty.clone(),
                            directives: Default::default(),
                        }));
                    }
                    Occupied(_i) => {
                        // TODO check description
                        // TODO check type
                        // TODO check default value
                        // TODO process directives
                    }
                }
            }
        } else {
            // TODO conflict on type
        }
    }

    fn merge_object_type(
        &mut self,
        types: &mut IndexMap<NamedType, ExtendedType>,
        metadata: &Option<&LinksMetadata>,
        subgraph_name: Name,
        object_name: NamedType,
        object: &Node<ObjectType>,
    ) {
        let federation_identity =
            metadata.and_then(|m| m.by_identity.get(&Identity::federation_identity()));

        let key_directive_name = federation_identity
            .map(|link| link.directive_name_in_schema(&FEDERATION_KEY_DIRECTIVE_NAME_IN_SPEC))
            .unwrap_or(FEDERATION_KEY_DIRECTIVE_NAME_IN_SPEC);

        let requires_directive_name = federation_identity
            .map(|link| link.directive_name_in_schema(&FEDERATION_REQUIRES_DIRECTIVE_NAME_IN_SPEC))
            .unwrap_or(FEDERATION_REQUIRES_DIRECTIVE_NAME_IN_SPEC);

        let provides_directive_name = federation_identity
            .map(|link| link.directive_name_in_schema(&FEDERATION_PROVIDES_DIRECTIVE_NAME_IN_SPEC))
            .unwrap_or(FEDERATION_PROVIDES_DIRECTIVE_NAME_IN_SPEC);

        let external_directive_name = federation_identity
            .map(|link| link.directive_name_in_schema(&FEDERATION_EXTERNAL_DIRECTIVE_NAME_IN_SPEC))
            .unwrap_or(FEDERATION_EXTERNAL_DIRECTIVE_NAME_IN_SPEC);

        let interface_object_directive_name = federation_identity
            .map(|link| {
                link.directive_name_in_schema(&FEDERATION_INTERFACEOBJECT_DIRECTIVE_NAME_IN_SPEC)
            })
            .unwrap_or(FEDERATION_INTERFACEOBJECT_DIRECTIVE_NAME_IN_SPEC);

        let authenticated_directive_name = federation_identity
            .map(|link| link.directive_name_in_schema(&name!("authenticated")))
            .unwrap_or(name!("authenticated"));

        let is_interface_object = object.directives.has(&interface_object_directive_name);
        let existing_type = types
            .entry(object_name.clone())
            .or_insert(copy_object_type_stub(
                object_name.clone(),
                object,
                is_interface_object,
            ));

        if let ExtendedType::Object(obj) = existing_type {
            let key_directives = object.directives.get_all(&key_directive_name);
            let join_type_directives =
                join_type_applied_directive(subgraph_name.clone(), key_directives, false);
            let mutable_object = obj.make_mut();
            mutable_object.directives.extend(join_type_directives);
            self.merge_descriptions(&mut mutable_object.description, &object.description);
            object.implements_interfaces.iter().for_each(|intf_name| {
                // IndexSet::insert deduplicates
                mutable_object
                    .implements_interfaces
                    .insert(intf_name.clone());
                let join_implements_directive =
                    join_implements_applied_directive(subgraph_name.clone(), intf_name);
                mutable_object.directives.push(join_implements_directive);
            });

            for (field_name, field) in object.fields.iter() {
                // skip federation built-in queries
                if field_name == "_service" || field_name == "_entities" {
                    continue;
                }

                let existing_field = mutable_object.fields.entry(field_name.clone());
                let supergraph_field = match existing_field {
                    Occupied(f) => {
                        // check description
                        // check type
                        // check args
                        f.into_mut()
                    }
                    Vacant(f) => f.insert(Component::new(FieldDefinition {
                        name: field.name.clone(),
                        description: field.description.clone(),
                        arguments: vec![],
                        directives: Default::default(),
                        ty: field.ty.clone(),
                    })),
                };

                self.merge_descriptions(
                    &mut supergraph_field.make_mut().description,
                    &field.description,
                );
                for arg in field.arguments.iter() {
                    if let Some(_existing_arg) = supergraph_field.argument_by_name(&arg.name) {
                        // TODO add args
                    } else {
                        // TODO mismatch no args
                    }
                }

                let requires_directive_option = Option::and_then(
                    field.directives.get_all(&requires_directive_name).next(),
                    |p| {
                        let requires_fields =
                            directive_string_arg_value(p, &name!("fields")).unwrap();
                        Some(requires_fields.as_str())
                    },
                );

                let provides_directive_option = Option::and_then(
                    field.directives.get_all(&provides_directive_name).next(),
                    |p| {
                        let provides_fields =
                            directive_string_arg_value(p, &name!("fields")).unwrap();
                        Some(provides_fields.as_str())
                    },
                );

                let external_field = field
                    .directives
                    .get_all(&external_directive_name)
                    .next()
                    .is_some();

                let join_field_directive = join_field_applied_directive(
                    subgraph_name.clone(),
                    requires_directive_option,
                    provides_directive_option,
                    external_field,
                );

                supergraph_field
                    .make_mut()
                    .directives
                    .push(Node::new(join_field_directive));

                dbg!(&field.directives);
                if let Some(authenticated_directive) = field
                    .directives
                    .get_all(&authenticated_directive_name)
                    .next()
                {
                    dbg!(&authenticated_directive);
                    supergraph_field
                        .make_mut()
                        .directives
                        .push(authenticated_directive.clone());
                }

                // TODO: implement needsJoinField to avoid adding join__field when unnecessary
                // https://github.com/apollographql/federation/blob/0d8a88585d901dff6844fdce1146a4539dec48df/composition-js/src/merging/merge.ts#L1648
            }
        } else if let ExtendedType::Interface(intf) = existing_type {
            // TODO support interface object
            let key_directives = object.directives.get_all(&key_directive_name);
            let join_type_directives =
                join_type_applied_directive(subgraph_name, key_directives, true);
            intf.make_mut().directives.extend(join_type_directives);
        };
        // TODO merge fields
    }

    fn merge_union_type(
        &mut self,
        types: &mut IndexMap<NamedType, ExtendedType>,
        subgraph_name: Name,
        union_name: NamedType,
        union: &Node<UnionType>,
    ) {
        let existing_type = types.entry(union_name.clone()).or_insert(copy_union_type(
            union_name.clone(),
            union.description.clone(),
        ));
        if let ExtendedType::Union(u) = existing_type {
            let join_type_directives =
                join_type_applied_directive(subgraph_name.clone(), iter::empty(), false);
            u.make_mut().directives.extend(join_type_directives);

            for union_member in union.members.iter() {
                // IndexSet::insert deduplicates
                u.make_mut().members.insert(union_member.clone());
                u.make_mut().directives.push(Component::new(Directive {
                    name: name!("join__unionMember"),
                    arguments: vec![
                        Node::new(Argument {
                            name: name!("graph"),
                            value: Node::new(Value::Enum(subgraph_name.clone())),
                        }),
                        Node::new(Argument {
                            name: name!("member"),
                            value: Node::new(Value::String(NodeStr::new(union_member))),
                        }),
                    ],
                }));
            }
        }
    }
}

const EXECUTABLE_DIRECTIVE_LOCATIONS: [DirectiveLocation; 8] = [
    DirectiveLocation::Query,
    DirectiveLocation::Mutation,
    DirectiveLocation::Subscription,
    DirectiveLocation::Field,
    DirectiveLocation::FragmentDefinition,
    DirectiveLocation::FragmentSpread,
    DirectiveLocation::InlineFragment,
    DirectiveLocation::VariableDefinition,
];
fn is_executable_directive(directive: &Node<DirectiveDefinition>) -> bool {
    directive
        .locations
        .iter()
        .any(|loc| EXECUTABLE_DIRECTIVE_LOCATIONS.contains(loc))
}

// TODO handle federation specific types - skip if any of the link/fed spec
// TODO this info should be coming from other module
const FEDERATION_TYPES: [&str; 4] = ["_Any", "_Entity", "_Service", "@key"];
fn is_mergeable_type(type_name: &str) -> bool {
    if type_name.starts_with("federation__") || type_name.starts_with("link__") {
        return false;
    }
    !FEDERATION_TYPES.contains(&type_name)
}

fn copy_enum_type(enum_name: Name, enum_type: &Node<EnumType>) -> ExtendedType {
    ExtendedType::Enum(Node::new(EnumType {
        description: enum_type.description.clone(),
        name: enum_name,
        directives: Default::default(),
        values: IndexMap::new(),
    }))
}

fn copy_input_object_type(
    input_object_name: Name,
    input_object: &Node<InputObjectType>,
) -> ExtendedType {
    let mut new_input_object = InputObjectType {
        description: input_object.description.clone(),
        name: input_object_name,
        directives: Default::default(),
        fields: IndexMap::new(),
    };

    for (field_name, input_field) in input_object.fields.iter() {
        new_input_object.fields.insert(
            field_name.clone(),
            Component::new(InputValueDefinition {
                name: input_field.name.clone(),
                description: input_field.description.clone(),
                directives: Default::default(),
                ty: input_field.ty.clone(),
                default_value: input_field.default_value.clone(),
            }),
        );
    }

    ExtendedType::InputObject(Node::new(new_input_object))
}

fn copy_interface_type(interface_name: Name, interface: &Node<InterfaceType>) -> ExtendedType {
    let new_interface = InterfaceType {
        description: interface.description.clone(),
        name: interface_name,
        directives: Default::default(),
        fields: copy_fields(interface.fields.iter()),
        implements_interfaces: interface.implements_interfaces.clone(),
    };
    ExtendedType::Interface(Node::new(new_interface))
}

fn copy_object_type_stub(
    object_name: Name,
    object: &Node<ObjectType>,
    is_interface_object: bool,
) -> ExtendedType {
    if is_interface_object {
        let new_interface = InterfaceType {
            description: object.description.clone(),
            name: object_name,
            directives: Default::default(),
            fields: copy_fields(object.fields.iter()),
            implements_interfaces: object.implements_interfaces.clone(),
        };
        ExtendedType::Interface(Node::new(new_interface))
    } else {
        let new_object = ObjectType {
            description: object.description.clone(),
            name: object_name,
            directives: Default::default(),
            fields: copy_fields(object.fields.iter()),
            implements_interfaces: object.implements_interfaces.clone(),
        };
        ExtendedType::Object(Node::new(new_object))
    }
}

fn copy_fields(
    fields_to_copy: Iter<Name, Component<FieldDefinition>>,
) -> IndexMap<Name, Component<FieldDefinition>> {
    let mut new_fields: IndexMap<Name, Component<FieldDefinition>> = IndexMap::new();
    for (field_name, field) in fields_to_copy {
        // skip federation built-in queries
        if field_name == "_service" || field_name == "_entities" {
            continue;
        }
        let args: Vec<Node<InputValueDefinition>> = field
            .arguments
            .iter()
            .map(|a| {
                Node::new(InputValueDefinition {
                    name: a.name.clone(),
                    description: a.description.clone(),
                    directives: Default::default(),
                    ty: a.ty.clone(),
                    default_value: a.default_value.clone(),
                })
            })
            .collect();
        let new_field = Component::new(FieldDefinition {
            name: field.name.clone(),
            description: field.description.clone(),
            directives: Default::default(),
            arguments: args,
            ty: field.ty.clone(),
        });

        new_fields.insert(field_name.clone(), new_field);
    }
    new_fields
}

fn copy_union_type(union_name: Name, description: Option<NodeStr>) -> ExtendedType {
    ExtendedType::Union(Node::new(UnionType {
        description,
        name: union_name,
        directives: Default::default(),
        members: IndexSet::new(),
    }))
}

fn join_type_applied_directive<'a>(
    subgraph_name: Name,
    key_directives: impl Iterator<Item = &'a Component<Directive>> + Sized,
    is_interface_object: bool,
) -> Vec<Component<Directive>> {
    let mut join_type_directive = Directive {
        name: name!("join__type"),
        arguments: vec![Node::new(Argument {
            name: name!("graph"),
            value: Node::new(Value::Enum(subgraph_name)),
        })],
    };
    if is_interface_object {
        join_type_directive.arguments.push(Node::new(Argument {
            name: name!("isInterfaceObject"),
            value: Node::new(Value::Boolean(is_interface_object)),
        }));
    }

    let mut result = vec![];
    for key_directive in key_directives {
        let mut join_type_directive_with_key = join_type_directive.clone();
        let field_set = directive_string_arg_value(key_directive, &name!("fields")).unwrap();
        join_type_directive_with_key
            .arguments
            .push(Node::new(Argument {
                name: name!("key"),
                value: Node::new(Value::String(NodeStr::new(field_set.as_str()))),
            }));

        let resolvable =
            directive_bool_arg_value(key_directive, &name!("resolvable")).unwrap_or(&true);
        if !resolvable {
            join_type_directive_with_key
                .arguments
                .push(Node::new(Argument {
                    name: name!("resolvable"),
                    value: Node::new(Value::Boolean(false)),
                }));
        }
        result.push(join_type_directive_with_key)
    }
    if result.is_empty() {
        result.push(join_type_directive)
    }
    result
        .into_iter()
        .map(Component::new)
        .collect::<Vec<Component<Directive>>>()
}

fn join_implements_applied_directive(
    subgraph_name: Name,
    intf_name: &Name,
) -> Component<Directive> {
    Component::new(Directive {
        name: name!("join__implements"),
        arguments: vec![
            Node::new(Argument {
                name: name!("graph"),
                value: Node::new(Value::Enum(subgraph_name)),
            }),
            Node::new(Argument {
                name: name!("interface"),
                value: Node::new(Value::String(intf_name.to_string().into())),
            }),
        ],
    })
}

fn directive_arg_value<'a>(directive: &'a Directive, arg_name: &Name) -> Option<&'a Value> {
    directive
        .arguments
        .iter()
        .find(|arg| arg.name == *arg_name)
        .map(|arg| arg.value.as_ref())
}

fn directive_string_arg_value<'a>(
    directive: &'a Directive,
    arg_name: &Name,
) -> Option<&'a NodeStr> {
    match directive_arg_value(directive, arg_name) {
        Some(Value::String(value)) => Some(value),
        _ => None,
    }
}

fn directive_bool_arg_value<'a>(directive: &'a Directive, arg_name: &Name) -> Option<&'a bool> {
    match directive_arg_value(directive, arg_name) {
        Some(Value::Boolean(value)) => Some(value),
        _ => None,
    }
}

// TODO use apollo_compiler::executable::FieldSet
fn parse_keys<'a>(
    directives: impl Iterator<Item = &'a Component<Directive>> + Sized,
) -> HashSet<&'a str> {
    HashSet::from_iter(
        directives
            .flat_map(|k| {
                let field_set = directive_string_arg_value(k, &name!("fields")).unwrap();
                field_set.split_whitespace()
            })
            .collect::<Vec<&str>>(),
    )
}

fn merge_directive(
    supergraph_directives: &mut IndexMap<Name, Node<DirectiveDefinition>>,
    directive: &Node<DirectiveDefinition>,
) {
    if !supergraph_directives.contains_key(&directive.name.clone()) {
        supergraph_directives.insert(directive.name.clone(), directive.clone());
    }
}

#[cfg(test)]
mod tests {
    use std::fs::read_to_string;

    use apollo_compiler::Schema;
    use insta::assert_snapshot;
    use insta::glob;

    use crate::merge::merge_federation_subgraphs;
    use crate::query_graph::extract_subgraphs_from_supergraph::extract_subgraphs_from_supergraph;
    use crate::schema::FederationSchema;
    use crate::schema::ValidFederationSchema;
    use crate::ValidFederationSubgraph;
    use crate::ValidFederationSubgraphs;

    #[test]
    fn test_steel_thread() {
        let one_sdl =
            include_str!("./sources/connect/expand/merge/connector_Query_users_0.graphql");
        let two_sdl = include_str!("./sources/connect/expand/merge/connector_Query_user_0.graphql");
        let three_sdl = include_str!("./sources/connect/expand/merge/connector_User_d_1.graphql");
        let graphql_sdl = include_str!("./sources/connect/expand/merge/graphql.graphql");

        let mut subgraphs = ValidFederationSubgraphs::new();
        subgraphs
            .add(ValidFederationSubgraph {
                name: "connector_Query_users_0".to_string(),
                url: "".to_string(),
                schema: ValidFederationSchema::new(
                    Schema::parse_and_validate(one_sdl, "./connector_Query_users_0.graphql")
                        .unwrap(),
                )
                .unwrap(),
            })
            .unwrap();
        subgraphs
            .add(ValidFederationSubgraph {
                name: "connector_Query_user_0".to_string(),
                url: "".to_string(),
                schema: ValidFederationSchema::new(
                    Schema::parse_and_validate(two_sdl, "./connector_Query_user_0.graphql")
                        .unwrap(),
                )
                .unwrap(),
            })
            .unwrap();
        subgraphs
            .add(ValidFederationSubgraph {
                name: "connector_User_d_1".to_string(),
                url: "".to_string(),
                schema: ValidFederationSchema::new(
                    Schema::parse_and_validate(three_sdl, "./connector_User_d_1.graphql").unwrap(),
                )
                .unwrap(),
            })
            .unwrap();
        subgraphs
            .add(ValidFederationSubgraph {
                name: "graphql".to_string(),
                url: "".to_string(),
                schema: ValidFederationSchema::new(
                    Schema::parse_and_validate(graphql_sdl, "./graphql.graphql").unwrap(),
                )
                .unwrap(),
            })
            .unwrap();

        let result = merge_federation_subgraphs(subgraphs).unwrap();

        let schema = result.schema.into_inner();
        let validation = schema.clone().validate();
        assert!(validation.is_ok(), "{:?}", validation);

        assert_snapshot!(schema.serialize());
    }

    #[test]
    fn test_round_trips() {
        insta::with_settings!({prepend_module_to_snapshot => false}, {
            glob!("../tests/test_data", "merge/roundtrip/*.graphql", |path| {
                let schema = read_to_string(path).unwrap();
                let parsed = Schema::parse(&schema, path).unwrap();
                let _sorted_string = sort_schema(&parsed).serialize().to_string();

                let federated = FederationSchema::new(parsed).unwrap();
                let subgraphs = extract_subgraphs_from_supergraph(&federated, None).unwrap();
                for (_, subgraph) in subgraphs.clone().into_iter() {
                    println!("{}", &subgraph.schema.schema().serialize().to_string())
                }
                let result = merge_federation_subgraphs(subgraphs).unwrap();

                // TODO: switch from snapshot tests to comparison tests when we
                // fix things like join__field optimizations

                // let sorted_result = sort_schema(&result.schema);
                // pretty_assertions::assert_eq!(_sorted_string, sorted_result.serialize().to_string());
                assert_snapshot!(result.schema.serialize());
            });
        });
    }

    fn sort_schema(schema: &Schema) -> Schema {
        let mut sorted = Schema::new();
        sorted.schema_definition = schema.schema_definition.clone();
        sorted.types = schema.types.clone();
        sorted.types.sort_keys();
        sorted.directive_definitions = schema.directive_definitions.clone();
        sorted.directive_definitions.sort_keys();
        sorted
    }
}
