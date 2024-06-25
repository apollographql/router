use apollo_compiler::ast::Argument;
use apollo_compiler::ast::Directive;
use apollo_compiler::ast::Name;
use apollo_compiler::ast::Value;
use apollo_compiler::name;
use apollo_compiler::Node;
use apollo_compiler::NodeStr;

use crate::error::FederationError;
use crate::link::inaccessible_spec_definition::INACCESSIBLE_DIRECTIVE_NAME_IN_SPEC;
use crate::link::spec::Identity;
use crate::link::spec::APOLLO_SPEC_DOMAIN;
use crate::link::Link;
use crate::schema::position::DirectiveArgumentDefinitionPosition;
use crate::schema::position::DirectiveDefinitionPosition;
use crate::schema::position::EnumTypeDefinitionPosition;
use crate::schema::position::EnumValueDefinitionPosition;
use crate::schema::position::InputObjectFieldDefinitionPosition;
use crate::schema::position::InputObjectTypeDefinitionPosition;
use crate::schema::position::InterfaceFieldArgumentDefinitionPosition;
use crate::schema::position::InterfaceFieldDefinitionPosition;
use crate::schema::position::InterfaceTypeDefinitionPosition;
use crate::schema::position::ObjectFieldArgumentDefinitionPosition;
use crate::schema::position::ObjectTypeDefinitionPosition;
use crate::schema::position::ScalarTypeDefinitionPosition;
use crate::schema::position::SchemaDefinitionPosition;
use crate::schema::position::UnionTypeDefinitionPosition;
use crate::schema::referencer::DirectiveReferencers;
use crate::schema::FederationSchema;
use crate::schema::ObjectFieldDefinitionPosition;

const TAG_DIRECTIVE_NAME_IN_SPEC: Name = name!("tag");
const AUTHENTICATED_DIRECTIVE_NAME_IN_SPEC: Name = name!("authenticated");
const REQUIRES_SCOPES_DIRECTIVE_NAME_IN_SPEC: Name = name!("requiresScopes");
const POLICY_DIRECTIVE_NAME_IN_SPEC: Name = name!("policy");

pub(super) fn carryover_directives(
    from: &FederationSchema,
    to: &mut FederationSchema,
) -> Result<(), FederationError> {
    let Some(metadata) = from.metadata() else {
        return Ok(());
    };

    // @inaccessible

    if let Some(link) = metadata.for_identity(&Identity::inaccessible_identity()) {
        let directive_name = link.directive_name_in_schema(&INACCESSIBLE_DIRECTIVE_NAME_IN_SPEC);
        from.referencers()
            .get_directive(&directive_name)
            .map(|referencers| {
                if referencers.len() > 0 {
                    SchemaDefinitionPosition
                        .insert_directive(to, link.to_directive_application().into())?;
                    copy_directive_definition(from, to, directive_name.clone())?;
                }
                referencers.copy_directives(from, to, &directive_name)
            })??;
    }

    // @tag

    if let Some(link) = metadata.for_identity(&Identity {
        domain: APOLLO_SPEC_DOMAIN.to_string(),
        name: TAG_DIRECTIVE_NAME_IN_SPEC,
    }) {
        let directive_name = link.directive_name_in_schema(&TAG_DIRECTIVE_NAME_IN_SPEC);
        from.referencers()
            .get_directive(&directive_name)
            .map(|referencers| {
                if referencers.len() > 0 {
                    SchemaDefinitionPosition
                        .insert_directive(to, link.to_directive_application().into())?;
                    copy_directive_definition(from, to, directive_name.clone())?;
                }
                referencers.copy_directives(from, to, &directive_name)
            })??;
    }

    // @authenticated

    if let Some(link) = metadata.for_identity(&Identity {
        domain: APOLLO_SPEC_DOMAIN.to_string(),
        name: AUTHENTICATED_DIRECTIVE_NAME_IN_SPEC,
    }) {
        let directive_name = link.directive_name_in_schema(&AUTHENTICATED_DIRECTIVE_NAME_IN_SPEC);
        from.referencers()
            .get_directive(&directive_name)
            .map(|referencers| {
                if referencers.len() > 0 {
                    SchemaDefinitionPosition
                        .insert_directive(to, link.to_directive_application().into())?;
                    copy_directive_definition(from, to, directive_name.clone())?;
                }
                referencers.copy_directives(from, to, &directive_name)
            })??;
    }

    // @requiresScopes

    if let Some(link) = metadata.for_identity(&Identity {
        domain: APOLLO_SPEC_DOMAIN.to_string(),
        name: REQUIRES_SCOPES_DIRECTIVE_NAME_IN_SPEC,
    }) {
        let directive_name = link.directive_name_in_schema(&REQUIRES_SCOPES_DIRECTIVE_NAME_IN_SPEC);
        from.referencers()
            .get_directive(&directive_name)
            .map(|referencers| {
                if referencers.len() > 0 {
                    SchemaDefinitionPosition
                        .insert_directive(to, link.to_directive_application().into())?;

                    let scalar_type_pos = ScalarTypeDefinitionPosition {
                        type_name: link.type_name_in_schema(&name!(Scope)),
                    };
                    scalar_type_pos.get(from.schema()).and_then(|def| {
                        scalar_type_pos.pre_insert(to)?;
                        scalar_type_pos.insert(to, def.clone())
                    })?;

                    copy_directive_definition(from, to, directive_name.clone())?;
                }
                referencers.copy_directives(from, to, &directive_name)
            })??;
    }

    // @policy

    if let Some(link) = metadata.for_identity(&Identity {
        domain: APOLLO_SPEC_DOMAIN.to_string(),
        name: POLICY_DIRECTIVE_NAME_IN_SPEC,
    }) {
        let directive_name = link.directive_name_in_schema(&POLICY_DIRECTIVE_NAME_IN_SPEC);
        from.referencers()
            .get_directive(&directive_name)
            .map(|referencers| {
                if referencers.len() > 0 {
                    SchemaDefinitionPosition
                        .insert_directive(to, link.to_directive_application().into())?;

                    let scalar_type_pos = ScalarTypeDefinitionPosition {
                        type_name: link.type_name_in_schema(&name!(Policy)),
                    };
                    scalar_type_pos.get(from.schema()).and_then(|def| {
                        scalar_type_pos.pre_insert(to)?;
                        scalar_type_pos.insert(to, def.clone())
                    })?;

                    copy_directive_definition(from, to, directive_name.clone())?;
                }
                referencers.copy_directives(from, to, &directive_name)
            })??;
    }

    // compose directive

    metadata
        .directives_by_imported_name
        .iter()
        .filter(|(_name, (link, _import))| !is_known_link(link))
        .try_for_each(|(name, (link, _import))| {
            let directive_name = link.directive_name_in_schema(name);
            from.referencers()
                .get_directive(&directive_name)
                .map(|referencers| {
                    if referencers.len() > 0 {
                        SchemaDefinitionPosition
                            .insert_directive(to, link.to_directive_application().into())?;
                        copy_directive_definition(from, to, directive_name.clone())?;
                    }
                    referencers.copy_directives(from, to, &directive_name)
                })??;
            Ok::<_, FederationError>(())
        })?;

    Ok(())
}

fn is_known_link(link: &Link) -> bool {
    link.url.identity.domain == APOLLO_SPEC_DOMAIN
        && [
            name!(link),
            name!(join),
            name!(tag),
            name!(inaccessible),
            name!(authenticated),
            name!(requiresScopes),
            name!(policy),
        ]
        .contains(&link.url.identity.name)
}

fn copy_directive_definition(
    from: &FederationSchema,
    to: &mut FederationSchema,
    directive_name: Name,
) -> Result<(), FederationError> {
    let def_pos = DirectiveDefinitionPosition { directive_name };

    def_pos.get(from.schema()).and_then(|def| {
        def_pos.pre_insert(to)?;
        def_pos.insert(to, def.clone())
    })
}

impl Link {
    fn to_directive_application(&self) -> Directive {
        let mut arguments: Vec<Node<Argument>> = vec![Argument {
            name: name!(url),
            value: self.url.to_string().into(),
        }
        .into()];

        // purpose: link__Purpose
        if let Some(purpose) = &self.purpose {
            arguments.push(
                Argument {
                    name: name!(purpose),
                    value: Value::Enum(purpose.into()).into(),
                }
                .into(),
            );
        }

        // as: String
        if let Some(alias) = &self.spec_alias {
            arguments.push(
                Argument {
                    name: name!(as),
                    value: Value::String(alias.clone().into()).into(),
                }
                .into(),
            );
        }

        // import: [link__Import!]
        if !self.imports.is_empty() {
            arguments.push(
                Argument {
                    name: name!(imports),
                    value: Value::List(
                        self.imports
                            .iter()
                            .map(|i| {
                                let name: NodeStr = if i.is_directive {
                                    format!("@{}", i.element).into()
                                } else {
                                    i.element.clone().into()
                                };

                                if let Some(alias) = &i.alias {
                                    let alias: NodeStr = if i.is_directive {
                                        format!("@{}", alias).into()
                                    } else {
                                        alias.clone().into()
                                    };

                                    Value::Object(vec![
                                        (name!(name), Value::String(name).into()),
                                        (name!(alias), Value::String(alias).into()),
                                    ])
                                } else {
                                    Value::String(name)
                                }
                                .into()
                            })
                            .collect::<Vec<_>>(),
                    )
                    .into(),
                }
                .into(),
            );
        }

        Directive {
            name: name!(link),
            arguments,
        }
    }
}

trait CopyDirective {
    fn copy_directive(
        &self,
        from: &FederationSchema,
        to: &mut FederationSchema,
        directive_name: &Name,
    ) -> Result<(), FederationError>;
}

impl CopyDirective for SchemaDefinitionPosition {
    fn copy_directive(
        &self,
        from: &FederationSchema,
        to: &mut FederationSchema,
        directive_name: &Name,
    ) -> Result<(), FederationError> {
        self.get(from.schema())
            .directives
            .iter()
            .filter(|d| &d.name == directive_name)
            .try_for_each(|directive| self.insert_directive(to, directive.clone()))
    }
}

macro_rules! impl_copy_directive {
    ($( $Ty: ty )+) => {
        $(
            impl CopyDirective for $Ty {
                fn copy_directive(
                    &self,
                    from: &FederationSchema,
                    to: &mut FederationSchema,
                    directive_name: &Name,
                ) -> Result<(), FederationError> {
                    self.get(from.schema())
                        .map(|def| {
                            def.directives
                                .iter()
                                .filter(|d| &d.name == directive_name)
                                .try_for_each(|directive| self.insert_directive(to, directive.clone()))
                        })
                        .unwrap_or(Ok(()))
                }
            }
        )+
    };
}

impl_copy_directive! {
    ScalarTypeDefinitionPosition
    ObjectTypeDefinitionPosition
    ObjectFieldDefinitionPosition
    ObjectFieldArgumentDefinitionPosition
    InterfaceTypeDefinitionPosition
    InterfaceFieldDefinitionPosition
    InterfaceFieldArgumentDefinitionPosition
    UnionTypeDefinitionPosition
    EnumTypeDefinitionPosition
    EnumValueDefinitionPosition
    InputObjectTypeDefinitionPosition
    InputObjectFieldDefinitionPosition
    DirectiveArgumentDefinitionPosition
}

impl DirectiveReferencers {
    fn len(&self) -> usize {
        self.schema.as_ref().map(|_| 1).unwrap_or_default()
            + self.scalar_types.len()
            + self.object_types.len()
            + self.object_fields.len()
            + self.object_field_arguments.len()
            + self.interface_types.len()
            + self.interface_fields.len()
            + self.interface_field_arguments.len()
            + self.union_types.len()
            + self.enum_types.len()
            + self.enum_values.len()
            + self.input_object_types.len()
            + self.input_object_fields.len()
            + self.directive_arguments.len()
    }

    fn copy_directives(
        &self,
        from: &FederationSchema,
        to: &mut FederationSchema,
        directive_name: &Name,
    ) -> Result<(), FederationError> {
        if let Some(position) = &self.schema {
            position.copy_directive(from, to, directive_name)?
        }
        self.scalar_types
            .iter()
            .try_for_each(|position| position.copy_directive(from, to, directive_name))?;
        self.object_types
            .iter()
            .try_for_each(|position| position.copy_directive(from, to, directive_name))?;
        self.object_fields
            .iter()
            .try_for_each(|position| position.copy_directive(from, to, directive_name))?;
        self.object_field_arguments
            .iter()
            .try_for_each(|position| position.copy_directive(from, to, directive_name))?;
        self.interface_types
            .iter()
            .try_for_each(|position| position.copy_directive(from, to, directive_name))?;
        self.interface_fields
            .iter()
            .try_for_each(|position| position.copy_directive(from, to, directive_name))?;
        self.interface_field_arguments
            .iter()
            .try_for_each(|position| position.copy_directive(from, to, directive_name))?;
        self.union_types
            .iter()
            .try_for_each(|position| position.copy_directive(from, to, directive_name))?;
        self.enum_types
            .iter()
            .try_for_each(|position| position.copy_directive(from, to, directive_name))?;
        self.enum_values
            .iter()
            .try_for_each(|position| position.copy_directive(from, to, directive_name))?;
        self.input_object_types
            .iter()
            .try_for_each(|position| position.copy_directive(from, to, directive_name))?;
        self.input_object_fields
            .iter()
            .try_for_each(|position| position.copy_directive(from, to, directive_name))?;
        self.directive_arguments
            .iter()
            .try_for_each(|position| position.copy_directive(from, to, directive_name))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use apollo_compiler::Schema;
    use insta::assert_snapshot;

    use super::carryover_directives;
    use crate::merge::merge_federation_subgraphs;
    use crate::query_graph::extract_subgraphs_from_supergraph::extract_subgraphs_from_supergraph;
    use crate::schema::FederationSchema;

    #[test]
    fn test_carryover() {
        let sdl = include_str!("./tests/schemas/directives.graphql");
        let schema = Schema::parse(sdl, "directives.graphql").expect("parse failed");
        let supergraph_schema = FederationSchema::new(schema).expect("federation schema failed");
        let subgraphs = extract_subgraphs_from_supergraph(&supergraph_schema, None)
            .expect("extract subgraphs failed");
        let merged = merge_federation_subgraphs(subgraphs).expect("merge failed");
        let schema = merged.schema.into_inner();
        let mut schema = FederationSchema::new(schema).expect("federation schema failed");

        carryover_directives(&supergraph_schema, &mut schema).expect("carryover failed");
        assert_snapshot!(schema.schema().serialize().to_string());
    }
}
