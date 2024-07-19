use std::sync::Arc;

use apollo_compiler::ast::Argument;
use apollo_compiler::ast::Directive;
use apollo_compiler::ast::DirectiveDefinition;
use apollo_compiler::ast::DirectiveLocation;
use apollo_compiler::ast::EnumValueDefinition;
use apollo_compiler::ast::FieldDefinition;
use apollo_compiler::ast::InputValueDefinition;
use apollo_compiler::ast::Type;
use apollo_compiler::ast::Value;
use apollo_compiler::name;
use apollo_compiler::schema::Component;
use apollo_compiler::schema::ComponentName;
use apollo_compiler::schema::EnumType;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::schema::ObjectType;
use apollo_compiler::schema::ScalarType;
use apollo_compiler::schema::UnionType;
use apollo_compiler::ty;
use apollo_compiler::InvalidNameError;
use apollo_compiler::Name;
use apollo_compiler::Node;
use indexmap::IndexMap;
use indexmap::IndexSet;
use lazy_static::lazy_static;
use thiserror::Error;

use crate::link::spec::Identity;
use crate::link::spec::Url;
use crate::link::spec::Version;
use crate::link::Import;
use crate::link::Link;
use crate::link::DEFAULT_IMPORT_SCALAR_NAME;
use crate::link::DEFAULT_LINK_NAME;
use crate::link::DEFAULT_PURPOSE_ENUM_NAME;
use crate::subgraph::spec::FederationSpecError::UnsupportedFederationDirective;
use crate::subgraph::spec::FederationSpecError::UnsupportedVersionError;

pub const COMPOSE_DIRECTIVE_NAME: Name = name!("composeDirective");
pub const KEY_DIRECTIVE_NAME: Name = name!("key");
pub const EXTENDS_DIRECTIVE_NAME: Name = name!("extends");
pub const EXTERNAL_DIRECTIVE_NAME: Name = name!("external");
pub const INACCESSIBLE_DIRECTIVE_NAME: Name = name!("inaccessible");
pub const INTF_OBJECT_DIRECTIVE_NAME: Name = name!("interfaceObject");
pub const OVERRIDE_DIRECTIVE_NAME: Name = name!("override");
pub const PROVIDES_DIRECTIVE_NAME: Name = name!("provides");
pub const REQUIRES_DIRECTIVE_NAME: Name = name!("requires");
pub const SHAREABLE_DIRECTIVE_NAME: Name = name!("shareable");
pub const TAG_DIRECTIVE_NAME: Name = name!("tag");
pub const FIELDSET_SCALAR_NAME: Name = name!("FieldSet");

// federated types
pub const ANY_SCALAR_NAME: Name = name!("_Any");
pub const ENTITY_UNION_NAME: Name = name!("_Entity");
pub const SERVICE_TYPE: Name = name!("_Service");

pub const ENTITIES_QUERY: Name = name!("_entities");
pub const SERVICE_SDL_QUERY: Name = name!("_service");

pub const FEDERATION_V1_DIRECTIVE_NAMES: [Name; 5] = [
    KEY_DIRECTIVE_NAME,
    EXTENDS_DIRECTIVE_NAME,
    EXTERNAL_DIRECTIVE_NAME,
    PROVIDES_DIRECTIVE_NAME,
    REQUIRES_DIRECTIVE_NAME,
];

pub const FEDERATION_V2_DIRECTIVE_NAMES: [Name; 11] = [
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

// This type and the subsequent IndexMap exist purely so we can use match with Names; see comment
// in FederationSpecDefinitions.directive_definition() for more information.
enum FederationDirectiveName {
    Compose,
    Key,
    Extends,
    External,
    Inaccessible,
    IntfObject,
    Override,
    Provides,
    Requires,
    Shareable,
    Tag,
}

lazy_static! {
    static ref FEDERATION_DIRECTIVE_NAMES_TO_ENUM: IndexMap<Name, FederationDirectiveName> = {
        IndexMap::from([
            (COMPOSE_DIRECTIVE_NAME, FederationDirectiveName::Compose),
            (KEY_DIRECTIVE_NAME, FederationDirectiveName::Key),
            (EXTENDS_DIRECTIVE_NAME, FederationDirectiveName::Extends),
            (EXTERNAL_DIRECTIVE_NAME, FederationDirectiveName::External),
            (
                INACCESSIBLE_DIRECTIVE_NAME,
                FederationDirectiveName::Inaccessible,
            ),
            (
                INTF_OBJECT_DIRECTIVE_NAME,
                FederationDirectiveName::IntfObject,
            ),
            (OVERRIDE_DIRECTIVE_NAME, FederationDirectiveName::Override),
            (PROVIDES_DIRECTIVE_NAME, FederationDirectiveName::Provides),
            (REQUIRES_DIRECTIVE_NAME, FederationDirectiveName::Requires),
            (SHAREABLE_DIRECTIVE_NAME, FederationDirectiveName::Shareable),
            (TAG_DIRECTIVE_NAME, FederationDirectiveName::Tag),
        ])
    };
}

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
    #[error(transparent)]
    InvalidGraphQLName(InvalidNameError),
}

impl From<InvalidNameError> for FederationSpecError {
    fn from(err: InvalidNameError) -> Self {
        FederationSpecError::InvalidGraphQLName(err)
    }
}

#[derive(Debug)]
pub struct FederationSpecDefinitions {
    link: Link,
    pub fieldset_scalar_name: Name,
}

#[derive(Debug)]
pub struct LinkSpecDefinitions {
    link: Link,
    pub import_scalar_name: Name,
    pub purpose_enum_name: Name,
}

pub trait AppliedFederationLink {
    fn applied_link_directive(&self) -> Directive;
}

macro_rules! applied_specification {
    ($($t:ty),+) => {
        $(impl AppliedFederationLink for $t {
            /// ```graphql
            /// @link(url: "https://specs.apollo.dev/federation/v2.3", import: ["@key"])
            /// ```
            fn applied_link_directive(&self) -> Directive {
                let imports = self
                    .link
                    .imports
                    .iter()
                    .map(|i| {
                        if i.alias.is_some() {
                            Value::Object(vec![
                                (name!("name"), i.element.as_str().into()),
                                (name!("as"), i.imported_display_name().to_string().into()),
                            ])
                        } else {
                            i.imported_display_name().to_string().into()
                        }.into()
                    })
                    .collect::<Vec<Node<Value>>>();
                let mut applied_link_directive = Directive {
                    name: DEFAULT_LINK_NAME,
                    arguments: vec![
                        Argument {
                            name: name!("url"),
                            value: self.link.url.to_string().into(),
                        }.into(),
                        Argument {
                            name: name!("import"),
                            value: Value::List(imports).into(),
                        }.into(),
                    ]
                };
                if let Some(spec_alias) = &self.link.spec_alias {
                    applied_link_directive.arguments.push(Argument {
                        name: name!("as"),
                        value: spec_alias.as_str().into(),
                    }.into())
                }
                if let Some(purpose) = &self.link.purpose {
                    applied_link_directive.arguments.push(Argument {
                        name: name!("for"),
                        value: Value::Enum(purpose.into()).into(),
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
            let fieldset_scalar_name = link.type_name_in_schema(&FIELDSET_SCALAR_NAME);
            Ok(Self {
                link,
                fieldset_scalar_name,
            })
        }
    }

    // The Default trait doesn't allow for returning Results, so we ignore the clippy warning here.
    #[allow(clippy::should_implement_trait)]
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
                        element: i.clone(),
                        alias: None,
                        is_directive: true,
                    })
                })
                .collect::<Vec<Arc<Import>>>(),
            purpose: None,
            spec_alias: None,
        })
    }

    pub fn namespaced_type_name(&self, name: &Name, is_directive: bool) -> Name {
        if is_directive {
            self.link.directive_name_in_schema(name)
        } else {
            self.link.type_name_in_schema(name)
        }
    }

    pub fn directive_definition(
        &self,
        name: &Name,
        alias: &Option<Name>,
    ) -> Result<DirectiveDefinition, FederationSpecError> {
        // TODO: `Name` has custom `PartialEq` and `Eq` impl so Clippy warns it should
        // not be used in pattern matching (as some future Rust version will likely turn this into
        // a hard error). We resort instead to indexing into a static IndexMap to get an enum, which
        // can be used in a match.
        let Some(enum_name) = FEDERATION_DIRECTIVE_NAMES_TO_ENUM.get(name) else {
            return Err(UnsupportedFederationDirective(name.to_string()));
        };
        Ok(match enum_name {
            FederationDirectiveName::Compose => self.compose_directive_definition(alias),
            FederationDirectiveName::Key => self.key_directive_definition(alias)?,
            FederationDirectiveName::Extends => self.extends_directive_definition(alias),
            FederationDirectiveName::External => self.external_directive_definition(alias),
            FederationDirectiveName::Inaccessible => self.inaccessible_directive_definition(alias),
            FederationDirectiveName::IntfObject => {
                self.interface_object_directive_definition(alias)
            }
            FederationDirectiveName::Override => self.override_directive_definition(alias),
            FederationDirectiveName::Provides => self.provides_directive_definition(alias)?,
            FederationDirectiveName::Requires => self.requires_directive_definition(alias)?,
            FederationDirectiveName::Shareable => self.shareable_directive_definition(alias),
            FederationDirectiveName::Tag => self.tag_directive_definition(alias),
        })
    }

    /// scalar FieldSet
    pub fn fieldset_scalar_definition(&self, name: Name) -> ScalarType {
        ScalarType {
            description: None,
            name,
            directives: Default::default(),
        }
    }

    fn fields_argument_definition(&self) -> Result<InputValueDefinition, FederationSpecError> {
        Ok(InputValueDefinition {
            description: None,
            name: name!("fields"),
            ty: Type::Named(self.namespaced_type_name(&FIELDSET_SCALAR_NAME, false))
                .non_null()
                .into(),
            default_value: None,
            directives: Default::default(),
        })
    }

    /// directive @composeDirective(name: String!) repeatable on SCHEMA
    fn compose_directive_definition(&self, alias: &Option<Name>) -> DirectiveDefinition {
        DirectiveDefinition {
            description: None,
            name: alias.clone().unwrap_or(COMPOSE_DIRECTIVE_NAME),
            arguments: vec![InputValueDefinition {
                description: None,
                name: name!("name"),
                ty: ty!(String!).into(),
                default_value: None,
                directives: Default::default(),
            }
            .into()],
            repeatable: true,
            locations: vec![DirectiveLocation::Schema],
        }
    }

    /// directive @key(fields: FieldSet!, resolvable: Boolean = true) repeatable on OBJECT | INTERFACE
    fn key_directive_definition(
        &self,
        alias: &Option<Name>,
    ) -> Result<DirectiveDefinition, FederationSpecError> {
        Ok(DirectiveDefinition {
            description: None,
            name: alias.clone().unwrap_or(KEY_DIRECTIVE_NAME),
            arguments: vec![
                self.fields_argument_definition()?.into(),
                InputValueDefinition {
                    description: None,
                    name: name!("resolvable"),
                    ty: ty!(Boolean).into(),
                    default_value: Some(true.into()),
                    directives: Default::default(),
                }
                .into(),
            ],
            repeatable: true,
            locations: vec![DirectiveLocation::Object, DirectiveLocation::Interface],
        })
    }

    /// directive @extends on OBJECT | INTERFACE
    fn extends_directive_definition(&self, alias: &Option<Name>) -> DirectiveDefinition {
        DirectiveDefinition {
            description: None,
            name: alias.clone().unwrap_or(EXTENDS_DIRECTIVE_NAME),
            arguments: Vec::new(),
            repeatable: false,
            locations: vec![DirectiveLocation::Object, DirectiveLocation::Interface],
        }
    }

    /// directive @external on OBJECT | FIELD_DEFINITION
    fn external_directive_definition(&self, alias: &Option<Name>) -> DirectiveDefinition {
        DirectiveDefinition {
            description: None,
            name: alias.clone().unwrap_or(EXTERNAL_DIRECTIVE_NAME),
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
    fn inaccessible_directive_definition(&self, alias: &Option<Name>) -> DirectiveDefinition {
        DirectiveDefinition {
            description: None,
            name: alias.clone().unwrap_or(INACCESSIBLE_DIRECTIVE_NAME),
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
    fn interface_object_directive_definition(&self, alias: &Option<Name>) -> DirectiveDefinition {
        DirectiveDefinition {
            description: None,
            name: alias.clone().unwrap_or(INTF_OBJECT_DIRECTIVE_NAME),
            arguments: Vec::new(),
            repeatable: false,
            locations: vec![DirectiveLocation::Object],
        }
    }

    /// directive @override(from: String!) on FIELD_DEFINITION
    fn override_directive_definition(&self, alias: &Option<Name>) -> DirectiveDefinition {
        DirectiveDefinition {
            description: None,
            name: alias.clone().unwrap_or(OVERRIDE_DIRECTIVE_NAME),
            arguments: vec![InputValueDefinition {
                description: None,
                name: name!("from"),
                ty: ty!(String!).into(),
                default_value: None,
                directives: Default::default(),
            }
            .into()],
            repeatable: false,
            locations: vec![DirectiveLocation::FieldDefinition],
        }
    }

    /// directive @provides(fields: FieldSet!) on FIELD_DEFINITION
    fn provides_directive_definition(
        &self,
        alias: &Option<Name>,
    ) -> Result<DirectiveDefinition, FederationSpecError> {
        Ok(DirectiveDefinition {
            description: None,
            name: alias.clone().unwrap_or(PROVIDES_DIRECTIVE_NAME),
            arguments: vec![self.fields_argument_definition()?.into()],
            repeatable: false,
            locations: vec![DirectiveLocation::FieldDefinition],
        })
    }

    /// directive @requires(fields: FieldSet!) on FIELD_DEFINITION
    fn requires_directive_definition(
        &self,
        alias: &Option<Name>,
    ) -> Result<DirectiveDefinition, FederationSpecError> {
        Ok(DirectiveDefinition {
            description: None,
            name: alias.clone().unwrap_or(REQUIRES_DIRECTIVE_NAME),
            arguments: vec![self.fields_argument_definition()?.into()],
            repeatable: false,
            locations: vec![DirectiveLocation::FieldDefinition],
        })
    }

    /// directive @shareable repeatable on FIELD_DEFINITION | OBJECT
    fn shareable_directive_definition(&self, alias: &Option<Name>) -> DirectiveDefinition {
        DirectiveDefinition {
            description: None,
            name: alias.clone().unwrap_or(SHAREABLE_DIRECTIVE_NAME),
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
    fn tag_directive_definition(&self, alias: &Option<Name>) -> DirectiveDefinition {
        DirectiveDefinition {
            description: None,
            name: alias.clone().unwrap_or(TAG_DIRECTIVE_NAME),
            arguments: vec![InputValueDefinition {
                description: None,
                name: name!("name"),
                ty: ty!(String!).into(),
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
            name: ANY_SCALAR_NAME,
            directives: Default::default(),
        };
        ExtendedType::Scalar(Node::new(any_scalar))
    }

    pub(crate) fn entity_union_definition(
        &self,
        entities: IndexSet<ComponentName>,
    ) -> ExtendedType {
        let service_type = UnionType {
            description: None,
            name: ENTITY_UNION_NAME,
            directives: Default::default(),
            members: entities,
        };
        ExtendedType::Union(Node::new(service_type))
    }
    pub(crate) fn service_object_type_definition(&self) -> ExtendedType {
        let mut service_type = ObjectType {
            description: None,
            name: SERVICE_TYPE,
            directives: Default::default(),
            fields: IndexMap::new(),
            implements_interfaces: IndexSet::new(),
        };
        service_type.fields.insert(
            name!("_sdl"),
            Component::new(FieldDefinition {
                name: name!("_sdl"),
                description: None,
                directives: Default::default(),
                arguments: Vec::new(),
                ty: ty!(String),
            }),
        );
        ExtendedType::Object(Node::new(service_type))
    }

    pub(crate) fn entities_query_field(&self) -> Component<FieldDefinition> {
        Component::new(FieldDefinition {
            name: ENTITIES_QUERY,
            description: None,
            directives: Default::default(),
            arguments: vec![Node::new(InputValueDefinition {
                name: name!("representations"),
                description: None,
                directives: Default::default(),
                ty: Node::new(Type::NonNullList(Box::new(Type::NonNullNamed(
                    ANY_SCALAR_NAME,
                )))),
                default_value: None,
            })],
            ty: Type::NonNullList(Box::new(Type::Named(ENTITY_UNION_NAME))),
        })
    }

    pub(crate) fn service_sdl_query_field(&self) -> Component<FieldDefinition> {
        Component::new(FieldDefinition {
            name: SERVICE_SDL_QUERY,
            description: None,
            directives: Default::default(),
            arguments: Vec::new(),
            ty: Type::NonNullNamed(SERVICE_TYPE),
        })
    }
}

impl LinkSpecDefinitions {
    pub fn new(link: Link) -> Self {
        let import_scalar_name = link.type_name_in_schema(&DEFAULT_IMPORT_SCALAR_NAME);
        let purpose_enum_name = link.type_name_in_schema(&DEFAULT_PURPOSE_ENUM_NAME);
        Self {
            link,
            import_scalar_name,
            purpose_enum_name,
        }
    }

    ///   scalar Import
    pub fn import_scalar_definition(&self, name: Name) -> ScalarType {
        ScalarType {
            description: None,
            name,
            directives: Default::default(),
        }
    }

    ///   enum link__Purpose {
    ///     SECURITY
    ///     EXECUTION
    ///   }
    pub fn link_purpose_enum_definition(&self, name: Name) -> EnumType {
        EnumType {
            description: None,
            name,
            directives: Default::default(),
            values: [
                (
                    name!("SECURITY"),
                    EnumValueDefinition {
                        description: None,
                        value: name!("SECURITY"),
                        directives: Default::default(),
                    }
                    .into(),
                ),
                (
                    name!("EXECUTION"),
                    EnumValueDefinition {
                        description: None,
                        value: name!("EXECUTION"),
                        directives: Default::default(),
                    }
                    .into(),
                ),
            ]
            .into(),
        }
    }

    ///   directive @link(url: String!, as: String, import: [Import], for: link__Purpose) repeatable on SCHEMA
    pub fn link_directive_definition(&self) -> Result<DirectiveDefinition, FederationSpecError> {
        Ok(DirectiveDefinition {
            description: None,
            name: DEFAULT_LINK_NAME,
            arguments: vec![
                InputValueDefinition {
                    description: None,
                    name: name!("url"),
                    ty: ty!(String!).into(),
                    default_value: None,
                    directives: Default::default(),
                }
                .into(),
                InputValueDefinition {
                    description: None,
                    name: name!("as"),
                    ty: ty!(String).into(),
                    default_value: None,
                    directives: Default::default(),
                }
                .into(),
                InputValueDefinition {
                    description: None,
                    name: name!("import"),
                    ty: Type::Named(self.import_scalar_name.clone()).list().into(),
                    default_value: None,
                    directives: Default::default(),
                }
                .into(),
                InputValueDefinition {
                    description: None,
                    name: name!("for"),
                    ty: Type::Named(self.purpose_enum_name.clone()).into(),
                    default_value: None,
                    directives: Default::default(),
                }
                .into(),
            ],
            repeatable: true,
            locations: vec![DirectiveLocation::Schema],
        })
    }
}

impl Default for LinkSpecDefinitions {
    fn default() -> Self {
        let link = Link {
            url: Url {
                identity: Identity::link_identity(),
                version: Version { major: 1, minor: 0 },
            },
            imports: vec![Arc::new(Import {
                element: name!("Import"),
                is_directive: false,
                alias: None,
            })],
            purpose: None,
            spec_alias: None,
        };
        Self::new(link)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subgraph::database::federation_link_identity;

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
