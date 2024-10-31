use apollo_compiler::ast::Argument;
use apollo_compiler::ast::Directive;
use apollo_compiler::collections::IndexMap;
use apollo_compiler::name;
use apollo_compiler::schema::Component;
use apollo_compiler::schema::EnumType;
use apollo_compiler::schema::ObjectType;
use apollo_compiler::schema::ScalarType;
use apollo_compiler::Name;
use apollo_compiler::Node;
use lazy_static::lazy_static;

use crate::error::FederationError;
use crate::link::spec::Identity;
use crate::link::spec::Url;
use crate::link::spec::Version;
use crate::link::spec_definition::SpecDefinition;
use crate::link::spec_definition::SpecDefinitions;
use crate::schema::position::EnumTypeDefinitionPosition;
use crate::schema::position::ObjectTypeDefinitionPosition;
use crate::schema::position::ScalarTypeDefinitionPosition;
use crate::schema::FederationSchema;

pub(crate) const CONTEXT_DIRECTIVE_NAME_IN_SPEC: Name = name!("context");
pub(crate) const CONTEXT_DIRECTIVE_NAME_DEFAULT: Name = name!("federation__context");

pub(crate) const FROM_CONTEXT_DIRECTIVE_NAME_IN_SPEC: Name = name!("fromContext");
pub(crate) const FROM_CONTEXT_DIRECTIVE_NAME_DEFAULT: Name = name!("federation__fromContext");

#[derive(Clone)]
pub(crate) struct ContextSpecDefinition {
    url: Url,
    minimum_federation_version: Option<Version>,
}

macro_rules! propagate_context_directives {
    ($func_name:ident, $directives_ty:ty, $wrap_ty:expr) => {
        pub(crate) fn $func_name(
            &self,
            subgraph_schema: &FederationSchema,
            source: &$directives_ty,
            dest: &mut $directives_ty,
            original_directive_names: &IndexMap<Name, Name>,
        ) -> Result<(), FederationError> {
            let context_directive_name =
                original_directive_names.get(&CONTEXT_DIRECTIVE_NAME_IN_SPEC);
            let context_directive =
                context_directive_name.and_then(|name| source.get(name.as_str()));
            if let Some(context_directive) = context_directive {
                dest.push($wrap_ty(self.context_directive(
                    subgraph_schema,
                    context_directive.arguments.clone(),
                )?));
            }

            let from_context_directive_name =
                original_directive_names.get(&FROM_CONTEXT_DIRECTIVE_NAME_DEFAULT);
            let from_context_directive =
                from_context_directive_name.and_then(|name| source.get(name.as_str()));
            if let Some(from_context_directive) = from_context_directive {
                dest.push($wrap_ty(self.from_context_directive(
                    subgraph_schema,
                    from_context_directive.arguments.clone(),
                )?));
            }

            Ok(())
        }
    };
}

macro_rules! propagate_context_directives_to_position {
    ($func_name:ident, $source_ty:ty, $dest_ty:ty) => {
        pub(crate) fn $func_name(
            &self,
            subgraph_schema: &mut FederationSchema,
            source: &Node<$source_ty>,
            dest: &$dest_ty,
            original_directive_names: &IndexMap<Name, Name>,
        ) -> Result<(), FederationError> {
            let context_directive_name =
                original_directive_names.get(&CONTEXT_DIRECTIVE_NAME_IN_SPEC);
            let context_directive =
                context_directive_name.and_then(|name| source.directives.get(name.as_str()));
            if let Some(context_directive) = context_directive {
                dest.insert_directive(
                    subgraph_schema,
                    Component::from(
                        self.context_directive(
                            subgraph_schema,
                            context_directive.arguments.clone(),
                        )?,
                    ),
                )?;
            }

            let from_context_directive_name =
                original_directive_names.get(&FROM_CONTEXT_DIRECTIVE_NAME_DEFAULT);
            let from_context_directive =
                from_context_directive_name.and_then(|name| source.directives.get(name.as_str()));
            if let Some(from_context_directive) = from_context_directive {
                dest.insert_directive(
                    subgraph_schema,
                    Component::from(self.from_context_directive(
                        subgraph_schema,
                        from_context_directive.arguments.clone(),
                    )?),
                )?;
            }

            Ok(())
        }
    };
}

impl ContextSpecDefinition {
    pub(crate) fn new(version: Version, minimum_federation_version: Option<Version>) -> Self {
        Self {
            url: Url {
                identity: Identity::context_identity(),
                version,
            },
            minimum_federation_version,
        }
    }

    pub(crate) fn context_directive_name_in_schema(
        &self,
        schema: &FederationSchema,
    ) -> Result<Name, FederationError> {
        Ok(self
            .directive_name_in_schema(schema, &CONTEXT_DIRECTIVE_NAME_IN_SPEC)?
            .unwrap_or(CONTEXT_DIRECTIVE_NAME_DEFAULT))
    }

    pub(crate) fn context_directive(
        &self,
        schema: &FederationSchema,
        arguments: Vec<Node<Argument>>,
    ) -> Result<Directive, FederationError> {
        let name = self
            .directive_name_in_schema(schema, &CONTEXT_DIRECTIVE_NAME_IN_SPEC)?
            .unwrap_or(CONTEXT_DIRECTIVE_NAME_DEFAULT);

        Ok(Directive { name, arguments })
    }

    pub(crate) fn from_context_directive(
        &self,
        schema: &FederationSchema,
        arguments: Vec<Node<Argument>>,
    ) -> Result<Directive, FederationError> {
        let name = self
            .directive_name_in_schema(schema, &FROM_CONTEXT_DIRECTIVE_NAME_IN_SPEC)?
            .unwrap_or(FROM_CONTEXT_DIRECTIVE_NAME_DEFAULT);

        Ok(Directive { name, arguments })
    }

    propagate_context_directives!(
        propagate_context_directives,
        apollo_compiler::ast::DirectiveList,
        Node::new
    );

    propagate_context_directives_to_position!(
        propagate_context_directives_for_enum,
        EnumType,
        EnumTypeDefinitionPosition
    );
    propagate_context_directives_to_position!(
        propagate_context_directives_for_object,
        ObjectType,
        ObjectTypeDefinitionPosition
    );
    propagate_context_directives_to_position!(
        propagate_context_directives_for_scalar,
        ScalarType,
        ScalarTypeDefinitionPosition
    );
}

impl SpecDefinition for ContextSpecDefinition {
    fn url(&self) -> &Url {
        &self.url
    }

    fn minimum_federation_version(&self) -> Option<&Version> {
        self.minimum_federation_version.as_ref()
    }
}

lazy_static! {
    pub(crate) static ref CONTEXT_VERSIONS: SpecDefinitions<ContextSpecDefinition> = {
        let mut definitions = SpecDefinitions::new(Identity::context_identity());
        definitions.add(ContextSpecDefinition::new(
            Version { major: 0, minor: 1 },
            Some(Version { major: 2, minor: 8 }),
        ));
        definitions
    };
}
