//! Implements API schema generation.
use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::Schema;
use apollo_compiler::ast;
use apollo_compiler::collections::IndexMap;
use apollo_compiler::collections::IndexSet;
use apollo_compiler::name;
use apollo_compiler::schema::DirectiveDefinition;
use apollo_compiler::schema::DirectiveLocation;
use apollo_compiler::schema::EnumType;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::schema::FieldDefinition;
use apollo_compiler::schema::InputObjectType;
use apollo_compiler::schema::InputValueDefinition;
use apollo_compiler::ty;

use crate::compat::coerce_and_validate_schema_values;
use crate::error::FederationError;
use crate::link::inaccessible_spec_definition::InaccessibleSpecDefinition;
use crate::schema::FederationSchema;
use crate::schema::ValidFederationSchema;

#[derive(Debug, Default, Clone)]
pub struct ApiSchemaOptions {
    pub include_defer: bool,
    pub include_stream: bool,
}

pub(crate) fn to_api_schema(
    schema: ValidFederationSchema,
    options: ApiSchemaOptions,
) -> Result<ValidFederationSchema, FederationError> {
    // Create a whole new federation schema that we can mutate.
    let mut api_schema = FederationSchema::from(schema);

    // As we compute the API schema of a supergraph, we want to ignore explicit definitions of `@defer` and `@stream` because
    // those correspond to the merging of potential definitions from the subgraphs, but whether the supergraph API schema
    // supports defer or not is unrelated to whether subgraphs support it.
    if let Some(defer) = api_schema.get_directive_definition(&name!("defer")) {
        defer.remove(&mut api_schema)?;
    }
    if let Some(stream) = api_schema.get_directive_definition(&name!("stream")) {
        stream.remove(&mut api_schema)?;
    }

    let inaccessible = match InaccessibleSpecDefinition::get_from_schema(&api_schema)? {
        Some(inaccessible_spec) => {
            inaccessible_spec.validate_inaccessible(&api_schema)?;
            Some(inaccessible_spec.directive_name(&api_schema)?)
        }
        None => None,
    };
    let hidden = HiddenElements::new(&api_schema, inaccessible);
    let mut schema = api_schema.into_inner();
    hidden.remove_from(&mut schema);

    if options.include_defer {
        schema
            .directive_definitions
            .insert(name!("defer"), defer_definition());
    }

    if options.include_stream {
        schema
            .directive_definitions
            .insert(name!("stream"), stream_definition());
    }

    crate::compat::make_print_schema_compatible(&mut schema);

    coerce_and_validate_schema_values(&mut schema)?;
    ValidFederationSchema::new(schema.validate()?)
}

fn defer_definition() -> Node<DirectiveDefinition> {
    Node::new(DirectiveDefinition {
        description: None,
        name: name!("defer"),
        arguments: vec![
            Node::new(InputValueDefinition {
                description: None,
                name: name!("label"),
                ty: ty!(String).into(),
                default_value: None,
                directives: Default::default(),
            }),
            Node::new(InputValueDefinition {
                description: None,
                name: name!("if"),
                ty: ty!(Boolean!).into(),
                default_value: Some(true.into()),
                directives: Default::default(),
            }),
        ],
        repeatable: false,
        locations: IndexSet::from_iter([
            DirectiveLocation::FragmentSpread,
            DirectiveLocation::InlineFragment,
        ]),
    })
}

fn stream_definition() -> Node<DirectiveDefinition> {
    Node::new(DirectiveDefinition {
        description: None,
        name: name!("stream"),
        arguments: vec![
            Node::new(InputValueDefinition {
                description: None,
                name: name!("label"),
                ty: ty!(String).into(),
                default_value: None,
                directives: Default::default(),
            }),
            Node::new(InputValueDefinition {
                description: None,
                name: name!("if"),
                ty: ty!(Boolean!).into(),
                default_value: Some(true.into()),
                directives: Default::default(),
            }),
            Node::new(InputValueDefinition {
                description: None,
                name: name!("initialCount"),
                ty: ty!(Int).into(),
                default_value: Some(0.into()),
                directives: Default::default(),
            }),
        ],
        repeatable: false,
        locations: IndexSet::from_iter([DirectiveLocation::Field]),
    })
}

/// Elements left out of the API schema: those marked `@inaccessible`, everything imported by
/// `@link`, and every element whose type is one of those.
struct HiddenElements {
    inaccessible: Option<Name>,
    types: IndexSet<Name>,
    directives: IndexSet<Name>,
}

impl HiddenElements {
    fn new(schema: &FederationSchema, inaccessible: Option<Name>) -> Self {
        let metadata = schema.metadata();
        let types = schema
            .schema()
            .types
            .iter()
            .filter(|(name, ty)| {
                inaccessible
                    .as_ref()
                    .is_some_and(|inaccessible| ty.directives().has(inaccessible))
                    || (!ty.is_built_in()
                        && metadata
                            .is_some_and(|metadata| metadata.source_link_of_type(name).is_some()))
            })
            .map(|(name, _)| name.clone())
            .collect();
        let directives = schema
            .schema()
            .directive_definitions
            .keys()
            .filter(|name| {
                metadata.is_some_and(|metadata| metadata.source_link_of_directive(name).is_some())
            })
            .cloned()
            .collect();
        Self {
            inaccessible,
            types,
            directives,
        }
    }

    // Removing elements through their positions updates schema-wide referencers on every
    // removal, which is quadratic when contracts hide most of the schema. Filtering the raw
    // schema needs no referencers, and `ValidFederationSchema::new` rebuilds them afterwards.
    fn remove_from(&self, schema: &mut Schema) {
        schema.types.retain(|name, _| !self.types.contains(name));
        for ty in schema.types.values_mut() {
            if !ty.is_built_in() {
                self.filter_type(ty);
            }
        }
        schema
            .directive_definitions
            .retain(|name, _| !self.directives.contains(name));
        for directive in schema.directive_definitions.values_mut() {
            if self.changes_input_values(&directive.arguments) {
                self.filter_input_values(&mut directive.make_mut().arguments);
            }
        }
        let definition = schema.schema_definition.make_mut();
        self.strip_directives(&mut definition.directives);
        for root in [
            &mut definition.query,
            &mut definition.mutation,
            &mut definition.subscription,
        ] {
            if root
                .as_ref()
                .is_some_and(|root| self.types.contains(&**root))
            {
                *root = None;
            }
        }
    }

    fn is_inaccessible(&self, directives: &ast::DirectiveList) -> bool {
        self.inaccessible
            .as_ref()
            .is_some_and(|inaccessible| directives.has(inaccessible))
    }

    fn hides(&self, directives: &ast::DirectiveList, ty: &Name) -> bool {
        self.is_inaccessible(directives) || self.types.contains(ty)
    }

    fn hides_input_value(&self, value: &InputValueDefinition) -> bool {
        self.hides(&value.directives, value.ty.inner_named_type())
    }

    fn hides_field(&self, field: &FieldDefinition) -> bool {
        self.hides(&field.directives, field.ty.inner_named_type())
    }

    fn has_hidden_directive(&self, directives: &ast::DirectiveList) -> bool {
        directives
            .iter()
            .any(|directive| self.directives.contains(&directive.name))
    }

    fn strip_directives(&self, directives: &mut ast::DirectiveList) {
        directives
            .0
            .retain(|directive| !self.directives.contains(&directive.name));
    }

    fn changes_input_values(&self, values: &[Node<InputValueDefinition>]) -> bool {
        values.iter().any(|value| {
            self.hides_input_value(value) || self.has_hidden_directive(&value.directives)
        })
    }

    fn filter_input_values(&self, values: &mut Vec<Node<InputValueDefinition>>) {
        values.retain(|value| !self.hides_input_value(value));
        for value in values.iter_mut() {
            if self.has_hidden_directive(&value.directives) {
                self.strip_directives(&mut value.make_mut().directives);
            }
        }
    }

    fn changes_field(&self, field: &FieldDefinition) -> bool {
        self.has_hidden_directive(&field.directives) || self.changes_input_values(&field.arguments)
    }

    fn changes_fields(&self, fields: &IndexMap<Name, Node<FieldDefinition>>) -> bool {
        fields
            .values()
            .any(|field| self.hides_field(field) || self.changes_field(field))
    }

    fn filter_fields(&self, fields: &mut IndexMap<Name, Node<FieldDefinition>>) {
        fields.retain(|_, field| !self.hides_field(field));
        for field in fields.values_mut() {
            if self.changes_field(field) {
                let field = field.make_mut();
                self.strip_directives(&mut field.directives);
                self.filter_input_values(&mut field.arguments);
            }
        }
    }

    fn hides_any_type(&self, names: &IndexSet<Node<Name>>) -> bool {
        names.iter().any(|name| self.types.contains(&**name))
    }

    fn filter_type_names(&self, names: &mut IndexSet<Node<Name>>) {
        names.retain(|name| !self.types.contains(&**name));
    }

    fn changes_fields_type(
        &self,
        directives: &ast::DirectiveList,
        fields: &IndexMap<Name, Node<FieldDefinition>>,
        implements_interfaces: &IndexSet<Node<Name>>,
    ) -> bool {
        self.has_hidden_directive(directives)
            || self.changes_fields(fields)
            || self.hides_any_type(implements_interfaces)
    }

    fn filter_fields_type(
        &self,
        directives: &mut ast::DirectiveList,
        fields: &mut IndexMap<Name, Node<FieldDefinition>>,
        implements_interfaces: &mut IndexSet<Node<Name>>,
    ) {
        self.strip_directives(directives);
        self.filter_fields(fields);
        self.filter_type_names(implements_interfaces);
    }

    fn filter_type(&self, ty: &mut ExtendedType) {
        match ty {
            ExtendedType::Scalar(scalar) => {
                if self.has_hidden_directive(&scalar.directives) {
                    self.strip_directives(&mut scalar.make_mut().directives);
                }
            }
            ExtendedType::Object(object) => {
                if self.changes_fields_type(
                    &object.directives,
                    &object.fields,
                    &object.implements_interfaces,
                ) {
                    let object = object.make_mut();
                    self.filter_fields_type(
                        &mut object.directives,
                        &mut object.fields,
                        &mut object.implements_interfaces,
                    );
                }
            }
            ExtendedType::Interface(interface) => {
                if self.changes_fields_type(
                    &interface.directives,
                    &interface.fields,
                    &interface.implements_interfaces,
                ) {
                    let interface = interface.make_mut();
                    self.filter_fields_type(
                        &mut interface.directives,
                        &mut interface.fields,
                        &mut interface.implements_interfaces,
                    );
                }
            }
            ExtendedType::Union(union_) => {
                if self.has_hidden_directive(&union_.directives)
                    || self.hides_any_type(&union_.members)
                {
                    let union_ = union_.make_mut();
                    self.strip_directives(&mut union_.directives);
                    self.filter_type_names(&mut union_.members);
                }
            }
            ExtendedType::Enum(enum_) => self.filter_enum(enum_),
            ExtendedType::InputObject(input_object) => self.filter_input_object(input_object),
        }
    }

    fn filter_enum(&self, enum_: &mut Node<EnumType>) {
        let changes_value = |directives: &ast::DirectiveList| {
            self.is_inaccessible(directives) || self.has_hidden_directive(directives)
        };
        if !self.has_hidden_directive(&enum_.directives)
            && !enum_
                .values
                .values()
                .any(|value| changes_value(&value.directives))
        {
            return;
        }
        let enum_ = enum_.make_mut();
        self.strip_directives(&mut enum_.directives);
        enum_
            .values
            .retain(|_, value| !self.is_inaccessible(&value.directives));
        for value in enum_.values.values_mut() {
            if self.has_hidden_directive(&value.directives) {
                self.strip_directives(&mut value.make_mut().directives);
            }
        }
    }

    fn filter_input_object(&self, input_object: &mut Node<InputObjectType>) {
        let changes_field = |field: &InputValueDefinition| {
            self.hides_input_value(field) || self.has_hidden_directive(&field.directives)
        };
        if !self.has_hidden_directive(&input_object.directives)
            && !input_object
                .fields
                .values()
                .any(|field| changes_field(field))
        {
            return;
        }
        let input_object = input_object.make_mut();
        self.strip_directives(&mut input_object.directives);
        input_object
            .fields
            .retain(|_, field| !self.hides_input_value(field));
        for field in input_object.fields.values_mut() {
            if self.has_hidden_directive(&field.directives) {
                self.strip_directives(&mut field.make_mut().directives);
            }
        }
    }
}
