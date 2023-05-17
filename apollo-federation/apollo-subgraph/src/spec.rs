use crate::spec::FederationSpecError::{UnsupportedFederationDirective, UnsupportedVersionError};
use apollo_at_link::link::{
    Import, Link, DEFAULT_IMPORT_SCALAR_NAME, DEFAULT_LINK_NAME, DEFAULT_PURPOSE_ENUM_NAME,
};
use apollo_at_link::spec::{Identity, Url, Version};
use apollo_compiler::ast::{
    Argument, Directive, DirectiveDefinition, DirectiveLocation, EnumValueDefinition,
    FieldDefinition, InputValueDefinition, Name, NamedType, Type, Value,
};
use apollo_compiler::schema::{
    Component, ComponentStr, EnumType, ExtendedType, ObjectType, ScalarType, UnionType,
};
use apollo_compiler::Node;
use indexmap::{IndexMap, IndexSet};
use std::sync::Arc;

use thiserror::Error;

pub const COMPOSE_DIRECTIVE_NAME: &str = "composeDirective";
pub const KEY_DIRECTIVE_NAME: &str = "key";
pub const EXTENDS_DIRECTIVE_NAME: &str = "extends";
pub const EXTERNAL_DIRECTIVE_NAME: &str = "external";
pub const INACCESSIBLE_DIRECTIVE_NAME: &str = "inaccessible";
pub const INTF_OBJECT_DIRECTIVE_NAME: &str = "interfaceObject";
pub const OVERRIDE_DIRECTIVE_NAME: &str = "override";
pub const PROVIDES_DIRECTIVE_NAME: &str = "provides";
pub const REQUIRES_DIRECTIVE_NAME: &str = "requires";
pub const SHAREABLE_DIRECTIVE_NAME: &str = "shareable";
pub const TAG_DIRECTIVE_NAME: &str = "tag";
pub const FIELDSET_SCALAR_NAME: &str = "FieldSet";

// federated types
pub const ANY_SCALAR_NAME: &str = "_Any";
pub const ENTITY_UNION_NAME: &str = "_Entity";
pub const SERVICE_TYPE: &str = "_Service";

pub const ENTITIES_QUERY: &str = "_entities";
pub const SERVICE_SDL_QUERY: &str = "_service";

pub const FEDERATION_V1_DIRECTIVE_NAMES: [&str; 5] = [
    KEY_DIRECTIVE_NAME,
    EXTENDS_DIRECTIVE_NAME,
    EXTERNAL_DIRECTIVE_NAME,
    PROVIDES_DIRECTIVE_NAME,
    REQUIRES_DIRECTIVE_NAME,
];

pub const FEDERATION_V2_DIRECTIVE_NAMES: [&str; 11] = [
    COMPOSE_DIRECTIVE_NAME,
    KEY_DIRECTIVE_NAME,
    EXTENDS_DIRECTIVE_NAME,
    EXTERNAL_DIRECTIVE_NAME,
    INACCESSIBLE_DIRECTIVE_NAME,
    INTF_OBJECT_DIRECTIVE_NAME,
    OVERRIDE_DIRECTIVE_NAME,
    PROVIDES_DIRECTIVE_NAME,
    REQUIRES_DIRECTIVE_NAME,
    SHAREABLE_DIRECTIVE_NAME,
    TAG_DIRECTIVE_NAME,
];

const MIN_FEDERATION_VERSION: Version = Version { major: 2, minor: 0 };
const MAX_FEDERATION_VERSION: Version = Version { major: 2, minor: 5 };

#[derive(Error, Debug, PartialEq)]
pub enum FederationSpecError {
    #[error(
        "Specified specification version {specified} is outside of supported range {min}-{max}"
    )]
    UnsupportedVersionError {
        specified: String,
        min: String,
        max: String,
    },
    #[error("Unsupported federation directive import {0}")]
    UnsupportedFederationDirective(String),
}

#[derive(Debug)]
pub struct FederationSpecDefinitions {
    link: Link,
    pub fieldset_scalar_name: String,
}

#[derive(Debug)]
pub struct LinkSpecDefinitions {
    link: Link,
    pub import_scalar_name: String,
    pub purpose_enum_name: String,
}

pub trait AppliedFederationLink {
    fn applied_link_directive(&self) -> Directive;
}

macro_rules! applied_specification {
    ($($t:ty),+) => {
        $(impl AppliedFederationLink for $t {
            /// @link(url: "https://specs.apollo.dev/federation/v2.3", import: ["@key"])
            fn applied_link_directive(&self) -> Directive {
                let imports = self
                    .link
                    .imports
                    .iter()
                    .map(|i| {
                        if i.alias.is_some() {
                            Value::Object(vec![
                                ("name".into(), i.element.as_str().into()),
                                ("as".into(), i.imported_display_name().into()),
                            ])
                        } else {
                            i.imported_display_name().into()
                        }.into()
                    })
                    .collect::<Vec<Node<Value>>>();
                let mut applied_link_directive = Directive {
                    name: DEFAULT_LINK_NAME.into(),
                    arguments: vec![
                        Argument {
                            name: "url".into(),
                            value: self.link.url.to_string().into(),
                        }.into(),
                        Argument {
                            name: "import".into(),
                            value: Value::List(imports).into(),
                        }.into(),
                    ]
                };
                if let Some(spec_alias) = &self.link.spec_alias {
                    applied_link_directive.arguments.push(Argument {
                        name: "as".into(),
                        value: spec_alias.into(),
                    }.into())
                }
                if let Some(purpose) = &self.link.purpose {
                    applied_link_directive.arguments.push(Argument {
                        name: "for".into(),
                        value: Value::Enum(purpose.to_string().into()).into(),
                    }.into())
                }
                applied_link_directive
            }
        })+
    }
}

applied_specification!(FederationSpecDefinitions, LinkSpecDefinitions);

impl FederationSpecDefinitions {
    pub fn from_link(link: Link) -> Result<Self, FederationSpecError> {
        if !link
            .url
            .version
            .satisfies_range(&MIN_FEDERATION_VERSION, &MAX_FEDERATION_VERSION)
        {
            Err(UnsupportedVersionError {
                specified: link.url.version.to_string(),
                min: MIN_FEDERATION_VERSION.to_string(),
                max: MAX_FEDERATION_VERSION.to_string(),
            })
        } else {
            let fieldset_scalar_name = link.type_name_in_schema(FIELDSET_SCALAR_NAME);
            Ok(Self {
                link,
                fieldset_scalar_name,
            })
        }
    }

    pub fn default() -> Result<Self, FederationSpecError> {
        Self::from_link(Link {
            url: Url {
                identity: Identity::federation_identity(),
                version: MAX_FEDERATION_VERSION,
            },
            imports: FEDERATION_V1_DIRECTIVE_NAMES
                .iter()
                .map(|i| {
                    Arc::new(Import {
                        element: i.to_string(),
                        alias: None,
                        is_directive: true,
                    })
                })
                .collect::<Vec<Arc<Import>>>(),
            purpose: None,
            spec_alias: None,
        })
    }

    pub fn namespaced_type_name(&self, name: &str, is_directive: bool) -> String {
        if is_directive {
            self.link.directive_name_in_schema(name)
        } else {
            self.link.type_name_in_schema(name)
        }
    }

    pub fn directive_definition(
        &self,
        name: &str,
        alias: &Option<String>,
    ) -> Result<DirectiveDefinition, FederationSpecError> {
        match name {
            COMPOSE_DIRECTIVE_NAME => Ok(self.compose_directive_definition(alias)),
            KEY_DIRECTIVE_NAME => Ok(self.key_directive_definition(alias)),
            EXTENDS_DIRECTIVE_NAME => Ok(self.extends_directive_definition(alias)),
            EXTERNAL_DIRECTIVE_NAME => Ok(self.external_directive_definition(alias)),
            INACCESSIBLE_DIRECTIVE_NAME => Ok(self.inaccessible_directive_definition(alias)),
            INTF_OBJECT_DIRECTIVE_NAME => Ok(self.interface_object_directive_definition(alias)),
            OVERRIDE_DIRECTIVE_NAME => Ok(self.override_directive_definition(alias)),
            PROVIDES_DIRECTIVE_NAME => Ok(self.provides_directive_definition(alias)),
            REQUIRES_DIRECTIVE_NAME => Ok(self.requires_directive_definition(alias)),
            SHAREABLE_DIRECTIVE_NAME => Ok(self.shareable_directive_definition(alias)),
            TAG_DIRECTIVE_NAME => Ok(self.tag_directive_definition(alias)),
            _ => Err(UnsupportedFederationDirective(name.to_string())),
        }
    }

    /// scalar FieldSet
    pub fn fieldset_scalar_definition(&self) -> ScalarType {
        ScalarType {
            description: None,
            directives: Default::default(),
        }
    }

    fn fields_argument_definition(&self) -> InputValueDefinition {
        InputValueDefinition {
            description: None,
            name: "fields".into(),
            ty: Type::new_named(&self.namespaced_type_name(FIELDSET_SCALAR_NAME, false))
                .non_null()
                .into(),
            default_value: None,
            directives: Default::default(),
        }
    }

    /// directive @composeDirective(name: String!) repeatable on SCHEMA
    fn compose_directive_definition(&self, alias: &Option<String>) -> DirectiveDefinition {
        DirectiveDefinition {
            description: None,
            name: alias.as_deref().unwrap_or(COMPOSE_DIRECTIVE_NAME).into(),
            arguments: vec![InputValueDefinition {
                description: None,
                name: "name".into(),
                ty: Type::new_named("String").non_null().into(),
                default_value: None,
                directives: Default::default(),
            }
            .into()],
            repeatable: true,
            locations: vec![DirectiveLocation::Schema],
        }
    }

    /// directive @key(fields: FieldSet!, resolvable: Boolean = true) repeatable on OBJECT | INTERFACE
    fn key_directive_definition(&self, alias: &Option<String>) -> DirectiveDefinition {
        DirectiveDefinition {
            description: None,
            name: alias.as_deref().unwrap_or(KEY_DIRECTIVE_NAME).into(),
            arguments: vec![
                self.fields_argument_definition().into(),
                InputValueDefinition {
                    description: None,
                    name: "resolvable".into(),
                    ty: Type::new_named("Boolean").into(),
                    default_value: Some(true.into()),
                    directives: Default::default(),
                }
                .into(),
            ],
            repeatable: true,
            locations: vec![DirectiveLocation::Object, DirectiveLocation::Interface],
        }
    }

    /// directive @extends on OBJECT | INTERFACE
    fn extends_directive_definition(&self, alias: &Option<String>) -> DirectiveDefinition {
        DirectiveDefinition {
            description: None,
            name: alias.as_deref().unwrap_or(EXTENDS_DIRECTIVE_NAME).into(),
            arguments: Vec::new(),
            repeatable: false,
            locations: vec![DirectiveLocation::Object, DirectiveLocation::Interface],
        }
    }

    /// directive @external on OBJECT | FIELD_DEFINITION
    fn external_directive_definition(&self, alias: &Option<String>) -> DirectiveDefinition {
        DirectiveDefinition {
            description: None,
            name: alias.as_deref().unwrap_or(EXTERNAL_DIRECTIVE_NAME).into(),
            arguments: Vec::new(),
            repeatable: false,
            locations: vec![
                DirectiveLocation::Object,
                DirectiveLocation::FieldDefinition,
            ],
        }
    }

    /// directive @inaccessible on
    ///   | ARGUMENT_DEFINITION
    ///   | ENUM
    ///   | ENUM_VALUE
    ///   | FIELD_DEFINITION
    ///   | INPUT_FIELD_DEFINITION
    ///   | INPUT_OBJECT
    ///   | INTERFACE
    ///   | OBJECT
    ///   | SCALAR
    ///   | UNION
    fn inaccessible_directive_definition(&self, alias: &Option<String>) -> DirectiveDefinition {
        DirectiveDefinition {
            description: None,
            name: alias
                .as_deref()
                .unwrap_or(INACCESSIBLE_DIRECTIVE_NAME)
                .into(),
            arguments: Vec::new(),
            repeatable: false,
            locations: vec![
                DirectiveLocation::ArgumentDefinition,
                DirectiveLocation::Enum,
                DirectiveLocation::EnumValue,
                DirectiveLocation::FieldDefinition,
                DirectiveLocation::InputFieldDefinition,
                DirectiveLocation::InputObject,
                DirectiveLocation::Interface,
                DirectiveLocation::Object,
                DirectiveLocation::Scalar,
                DirectiveLocation::Union,
            ],
        }
    }

    /// directive @interfaceObject on OBJECT
    fn interface_object_directive_definition(&self, alias: &Option<String>) -> DirectiveDefinition {
        DirectiveDefinition {
            description: None,
            name: alias
                .as_deref()
                .unwrap_or(INTF_OBJECT_DIRECTIVE_NAME)
                .into(),
            arguments: Vec::new(),
            repeatable: false,
            locations: vec![DirectiveLocation::Object],
        }
    }

    /// directive @override(from: String!) on FIELD_DEFINITION
    fn override_directive_definition(&self, alias: &Option<String>) -> DirectiveDefinition {
        DirectiveDefinition {
            description: None,
            name: alias.as_deref().unwrap_or(OVERRIDE_DIRECTIVE_NAME).into(),
            arguments: vec![InputValueDefinition {
                description: None,
                name: "from".into(),
                ty: Type::new_named("String").non_null().into(),
                default_value: None,
                directives: Default::default(),
            }
            .into()],
            repeatable: false,
            locations: vec![DirectiveLocation::FieldDefinition],
        }
    }

    /// directive @provides(fields: FieldSet!) on FIELD_DEFINITION
    fn provides_directive_definition(&self, alias: &Option<String>) -> DirectiveDefinition {
        DirectiveDefinition {
            description: None,
            name: alias.as_deref().unwrap_or(PROVIDES_DIRECTIVE_NAME).into(),
            arguments: vec![self.fields_argument_definition().into()],
            repeatable: false,
            locations: vec![DirectiveLocation::FieldDefinition],
        }
    }

    /// directive @requires(fields: FieldSet!) on FIELD_DEFINITION
    fn requires_directive_definition(&self, alias: &Option<String>) -> DirectiveDefinition {
        DirectiveDefinition {
            description: None,
            name: alias.as_deref().unwrap_or(REQUIRES_DIRECTIVE_NAME).into(),
            arguments: vec![self.fields_argument_definition().into()],
            repeatable: false,
            locations: vec![DirectiveLocation::FieldDefinition],
        }
    }

    /// directive @shareable repeatable on FIELD_DEFINITION | OBJECT
    fn shareable_directive_definition(&self, alias: &Option<String>) -> DirectiveDefinition {
        DirectiveDefinition {
            description: None,
            name: alias.as_deref().unwrap_or(SHAREABLE_DIRECTIVE_NAME).into(),
            arguments: Vec::new(),
            repeatable: true,
            locations: vec![
                DirectiveLocation::FieldDefinition,
                DirectiveLocation::Object,
            ],
        }
    }

    /// directive @tag(name: String!) repeatable on
    ///   | ARGUMENT_DEFINITION
    ///   | ENUM
    ///   | ENUM_VALUE
    ///   | FIELD_DEFINITION
    ///   | INPUT_FIELD_DEFINITION
    ///   | INPUT_OBJECT
    ///   | INTERFACE
    ///   | OBJECT
    ///   | SCALAR
    ///   | UNION
    fn tag_directive_definition(&self, alias: &Option<String>) -> DirectiveDefinition {
        DirectiveDefinition {
            description: None,
            name: alias.as_deref().unwrap_or(TAG_DIRECTIVE_NAME).into(),
            arguments: vec![InputValueDefinition {
                description: None,
                name: "name".into(),
                ty: Type::new_named("String").non_null().into(),
                default_value: None,
                directives: Default::default(),
            }
            .into()],
            repeatable: true,
            locations: vec![
                DirectiveLocation::ArgumentDefinition,
                DirectiveLocation::Enum,
                DirectiveLocation::EnumValue,
                DirectiveLocation::FieldDefinition,
                DirectiveLocation::InputFieldDefinition,
                DirectiveLocation::InputObject,
                DirectiveLocation::Interface,
                DirectiveLocation::Object,
                DirectiveLocation::Scalar,
                DirectiveLocation::Union,
            ],
        }
    }

    pub(crate) fn any_scalar_definition(&self) -> ExtendedType {
        let any_scalar = ScalarType {
            description: None,
            directives: Default::default(),
        };
        ExtendedType::Scalar(Node::new(any_scalar))
    }

    pub(crate) fn entity_union_definition(&self, entities: IndexSet<ComponentStr>) -> ExtendedType {
        let service_type = UnionType {
            description: None,
            directives: Default::default(),
            members: entities,
        };
        ExtendedType::Union(Node::new(service_type))
    }
    pub(crate) fn service_object_type_definition(&self) -> ExtendedType {
        let mut service_type = ObjectType {
            description: None,
            directives: Default::default(),
            fields: IndexMap::new(),
            implements_interfaces: IndexSet::new(),
        };
        service_type.fields.insert(
            Name::new("_sdl"),
            Component::new(FieldDefinition {
                name: Name::new("_sdl"),
                description: None,
                directives: Default::default(),
                arguments: Vec::new(),
                ty: Type::Named(NamedType::new("String")),
            }),
        );
        ExtendedType::Object(Node::new(service_type))
    }

    pub(crate) fn entities_query_field(&self) -> Component<FieldDefinition> {
        Component::new(FieldDefinition {
            name: Name::new(ENTITIES_QUERY),
            description: None,
            directives: Default::default(),
            arguments: vec![Node::new(InputValueDefinition {
                name: Name::new("representations"),
                description: None,
                directives: Default::default(),
                ty: Node::new(Type::NonNullList(Box::new(Type::NonNullNamed(
                    NamedType::new(ANY_SCALAR_NAME),
                )))),
                default_value: None,
            })],
            ty: Type::NonNullList(Box::new(Type::Named(NamedType::new(ENTITY_UNION_NAME)))),
        })
    }

    pub(crate) fn service_sdl_query_field(&self) -> Component<FieldDefinition> {
        Component::new(FieldDefinition {
            name: Name::new(SERVICE_SDL_QUERY),
            description: None,
            directives: Default::default(),
            arguments: Vec::new(),
            ty: Type::NonNullNamed(NamedType::new(SERVICE_TYPE)),
        })
    }
}

impl LinkSpecDefinitions {
    pub fn new(link: Link) -> Self {
        let import_scalar_name = link.type_name_in_schema(DEFAULT_IMPORT_SCALAR_NAME);
        let purpose_enum_name = link.type_name_in_schema(DEFAULT_PURPOSE_ENUM_NAME);
        Self {
            link,
            import_scalar_name,
            purpose_enum_name,
        }
    }

    pub fn default() -> Self {
        let link = Link {
            url: Url {
                identity: Identity::link_identity(),
                version: Version { major: 1, minor: 0 },
            },
            imports: vec![Arc::new(Import {
                element: "Import".to_owned(),
                is_directive: false,
                alias: None,
            })],
            purpose: None,
            spec_alias: None,
        };
        Self::new(link)
    }

    ///   scalar Import
    pub fn import_scalar_definition(&self) -> ScalarType {
        ScalarType {
            description: None,
            directives: Default::default(),
        }
    }

    ///   enum link__Purpose {
    ///     SECURITY
    ///     EXECUTION
    ///   }
    pub fn link_purpose_enum_definition(&self) -> EnumType {
        EnumType {
            description: None,
            directives: Default::default(),
            values: [
                (
                    "SECURITY".into(),
                    EnumValueDefinition {
                        description: None,
                        value: "SECURITY".into(),
                        directives: Default::default(),
                    }
                    .into(),
                ),
                (
                    "EXECUTION".into(),
                    EnumValueDefinition {
                        description: None,
                        value: "EXECUTION".into(),
                        directives: Default::default(),
                    }
                    .into(),
                ),
            ]
            .into(),
        }
    }

    ///   directive @link(url: String, as: String, import: [Import], for: link__Purpose) repeatable on SCHEMA
    pub fn link_directive_definition(&self) -> DirectiveDefinition {
        DirectiveDefinition {
            description: None,
            name: DEFAULT_LINK_NAME.into(),
            arguments: vec![
                InputValueDefinition {
                    description: None,
                    name: "url".into(),
                    // TODO: doc-comment disagrees with non-null here
                    ty: Type::new_named("String").non_null().into(),
                    default_value: None,
                    directives: Default::default(),
                }
                .into(),
                InputValueDefinition {
                    description: None,
                    name: "as".into(),
                    ty: Type::new_named("String").into(),
                    default_value: None,
                    directives: Default::default(),
                }
                .into(),
                InputValueDefinition {
                    description: None,
                    name: "import".into(),
                    ty: Type::new_named(&self.import_scalar_name).list().into(),
                    default_value: None,
                    directives: Default::default(),
                }
                .into(),
                InputValueDefinition {
                    description: None,
                    name: "for".into(),
                    ty: Type::new_named(&self.purpose_enum_name).into(),
                    default_value: None,
                    directives: Default::default(),
                }
                .into(),
            ],
            repeatable: true,
            locations: vec![DirectiveLocation::Schema],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::federation_link_identity;

    #[test]
    fn handle_unsupported_federation_version() {
        FederationSpecDefinitions::from_link(Link {
            url: Url {
                identity: federation_link_identity(),
                version: Version {
                    major: 99,
                    minor: 99,
                },
            },
            spec_alias: None,
            imports: vec![],
            purpose: None,
        })
        .expect_err("federation version 99 is not yet supported");
    }
}
