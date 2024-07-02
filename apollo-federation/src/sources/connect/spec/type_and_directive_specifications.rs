use apollo_compiler::ast::DirectiveLocation;
use apollo_compiler::ast::InputValueDefinition;
use apollo_compiler::ast::Type;
use apollo_compiler::ast::Value;
use apollo_compiler::name;
use apollo_compiler::schema::Component;
use apollo_compiler::schema::InputObjectType;
use apollo_compiler::ty;
use indexmap::IndexMap;

use super::schema::CONNECT_DIRECTIVE_NAME_IN_SPEC;
use super::schema::CONNECT_ENTITY_ARGUMENT_NAME;
use super::schema::CONNECT_HTTP_ARGUMENT_NAME;
use super::schema::CONNECT_HTTP_NAME_IN_SPEC;
use super::schema::CONNECT_SELECTION_ARGUMENT_NAME;
use super::schema::CONNECT_SOURCE_ARGUMENT_NAME;
use super::schema::HTTP_HEADER_MAPPING_NAME_IN_SPEC;
use super::schema::JSON_SELECTION_SCALAR_NAME;
use super::schema::SOURCE_DIRECTIVE_NAME_IN_SPEC;
use super::schema::SOURCE_HTTP_ARGUMENT_NAME;
use super::schema::SOURCE_HTTP_NAME_IN_SPEC;
use super::schema::SOURCE_NAME_ARGUMENT_NAME;
use super::schema::URL_PATH_TEMPLATE_SCALAR_NAME;
use super::ConnectSpecDefinition;
use crate::error::FederationError;
use crate::error::SingleFederationError;
use crate::link::Link;
use crate::schema::position::InputObjectTypeDefinitionPosition;
use crate::schema::type_and_directive_specification::ArgumentSpecification;
use crate::schema::type_and_directive_specification::DirectiveArgumentSpecification;
use crate::schema::type_and_directive_specification::DirectiveSpecification;
use crate::schema::type_and_directive_specification::ScalarTypeSpecification;
use crate::schema::type_and_directive_specification::TypeAndDirectiveSpecification;
use crate::schema::FederationSchema;
use crate::sources::connect::spec::schema::CONNECT_BODY_ARGUMENT_NAME;
use crate::sources::connect::spec::schema::CONNECT_HEADERS_ARGUMENT_NAME;
use crate::sources::connect::spec::schema::HTTP_HEADER_MAPPING_AS_ARGUMENT_NAME;
use crate::sources::connect::spec::schema::HTTP_HEADER_MAPPING_NAME_ARGUMENT_NAME;
use crate::sources::connect::spec::schema::HTTP_HEADER_MAPPING_VALUE_ARGUMENT_NAME;
use crate::sources::connect::spec::schema::SOURCE_BASE_URL_ARGUMENT_NAME;
use crate::sources::connect::spec::schema::SOURCE_HEADERS_ARGUMENT_NAME;

pub(super) fn check_or_add(
    link: &Link,
    schema: &mut FederationSchema,
) -> Result<(), FederationError> {
    fn internal(s: &str) -> SingleFederationError {
        SingleFederationError::Internal {
            message: s.to_string(),
        }
    }

    // scalar JSONSelection
    let json_selection_spec = ScalarTypeSpecification {
        name: link.type_name_in_schema(&JSON_SELECTION_SCALAR_NAME),
    };

    // scalar URLPathTemplate
    let url_path_template_spec = ScalarTypeSpecification {
        name: link.type_name_in_schema(&URL_PATH_TEMPLATE_SCALAR_NAME),
    };

    // -------------------------------------------------------------------------
    let http_header_mapping_field_list = vec![
        InputValueDefinition {
            description: None,
            name: HTTP_HEADER_MAPPING_NAME_ARGUMENT_NAME.clone(),
            ty: ty!(String!).into(),
            default_value: None,
            directives: Default::default(),
        },
        InputValueDefinition {
            description: None,
            name: HTTP_HEADER_MAPPING_AS_ARGUMENT_NAME.clone(),
            ty: ty!(String).into(),
            default_value: None,
            directives: Default::default(),
        },
        InputValueDefinition {
            description: None,
            name: HTTP_HEADER_MAPPING_VALUE_ARGUMENT_NAME.clone(),
            ty: ty!([String!]).into(),
            default_value: None,
            directives: Default::default(),
        },
    ];

    let mut http_header_mapping_fields = IndexMap::new();
    for field in http_header_mapping_field_list {
        http_header_mapping_fields.insert(field.name.clone(), Component::new(field));
    }

    // input HTTPHeaderMapping {
    //   name: String!
    //   as: String
    //   value: [String!]
    // }
    let http_header_mapping = InputObjectType {
        description: None,
        name: link.type_name_in_schema(&HTTP_HEADER_MAPPING_NAME_IN_SPEC),
        directives: Default::default(),
        fields: http_header_mapping_fields,
    };

    let http_header_mapping_pos = InputObjectTypeDefinitionPosition {
        type_name: http_header_mapping.name.clone(),
    };

    // -------------------------------------------------------------------------

    let connect_http_field_list = vec![
        InputValueDefinition {
            description: None,
            name: name!(GET),
            ty: Type::Named(url_path_template_spec.name.clone()).into(),
            default_value: None,
            directives: Default::default(),
        },
        InputValueDefinition {
            description: None,
            name: name!(POST),
            ty: Type::Named(url_path_template_spec.name.clone()).into(),
            default_value: None,
            directives: Default::default(),
        },
        InputValueDefinition {
            description: None,
            name: name!(PUT),
            ty: Type::Named(url_path_template_spec.name.clone()).into(),
            default_value: None,
            directives: Default::default(),
        },
        InputValueDefinition {
            description: None,
            name: name!(PATCH),
            ty: Type::Named(url_path_template_spec.name.clone()).into(),
            default_value: None,
            directives: Default::default(),
        },
        InputValueDefinition {
            description: None,
            name: name!(DELETE),
            ty: Type::Named(url_path_template_spec.name.clone()).into(),
            default_value: None,
            directives: Default::default(),
        },
        InputValueDefinition {
            description: None,
            name: CONNECT_BODY_ARGUMENT_NAME.clone(),
            ty: Type::Named(json_selection_spec.name.clone()).into(),
            default_value: None,
            directives: Default::default(),
        },
        InputValueDefinition {
            description: None,
            name: CONNECT_HEADERS_ARGUMENT_NAME.clone(),
            ty: Type::List(Box::new(Type::NonNullNamed(
                http_header_mapping.name.clone(),
            )))
            .into(),
            default_value: None,
            directives: Default::default(),
        },
    ];

    let mut connect_http_fields = IndexMap::new();
    for field in connect_http_field_list {
        connect_http_fields.insert(field.name.clone(), Component::new(field));
    }

    let connect_http = InputObjectType {
        name: link.type_name_in_schema(&CONNECT_HTTP_NAME_IN_SPEC),
        description: None,
        directives: Default::default(),
        fields: connect_http_fields,
    };

    let connect_http_pos = InputObjectTypeDefinitionPosition {
        type_name: connect_http.name.clone(),
    };

    // -------------------------------------------------------------------------

    // directive @connect(
    //   source: String
    //   http: ConnectHTTP
    //   selection: JSONSelection!
    //   entity: Boolean = false
    // ) repeatable on FIELD_DEFINITION
    let connect_spec = DirectiveSpecification::new(
        link.directive_name_in_schema(&CONNECT_DIRECTIVE_NAME_IN_SPEC),
        &[
            DirectiveArgumentSpecification {
                base_spec: ArgumentSpecification {
                    name: CONNECT_SOURCE_ARGUMENT_NAME.clone(),
                    get_type: |_| Ok(ty!(String)),
                    default_value: None,
                },
                composition_strategy: None,
            },
            DirectiveArgumentSpecification {
                base_spec: ArgumentSpecification {
                    name: CONNECT_HTTP_ARGUMENT_NAME.clone(),
                    get_type: |s| {
                        let name = s
                            .metadata()
                            .ok_or_else(|| internal("missing metadata"))?
                            .for_identity(&ConnectSpecDefinition::identity())
                            .ok_or_else(|| internal("missing connect spec"))?
                            .type_name_in_schema(&CONNECT_HTTP_NAME_IN_SPEC);
                        Ok(Type::Named(name))
                    },
                    default_value: None,
                },
                composition_strategy: None,
            },
            DirectiveArgumentSpecification {
                base_spec: ArgumentSpecification {
                    name: CONNECT_SELECTION_ARGUMENT_NAME.clone(),
                    get_type: |s| {
                        let name = s
                            .metadata()
                            .ok_or_else(|| internal("missing metadata"))?
                            .for_identity(&ConnectSpecDefinition::identity())
                            .ok_or_else(|| internal("missing connect spec"))?
                            .type_name_in_schema(&JSON_SELECTION_SCALAR_NAME);
                        Ok(Type::NonNullNamed(name))
                    },
                    default_value: None,
                },
                composition_strategy: None,
            },
            DirectiveArgumentSpecification {
                base_spec: ArgumentSpecification {
                    name: CONNECT_ENTITY_ARGUMENT_NAME.clone(),
                    get_type: |_| Ok(Type::Named(name!(Boolean))),
                    default_value: Some(Value::Boolean(false)),
                },
                composition_strategy: None,
            },
        ],
        true,
        &[DirectiveLocation::FieldDefinition],
        false,
        None,
    );

    // -------------------------------------------------------------------------

    let source_http_field_list = vec![
        InputValueDefinition {
            description: None,
            name: SOURCE_BASE_URL_ARGUMENT_NAME.clone(),
            ty: ty!(String!).into(),
            default_value: None,
            directives: Default::default(),
        },
        InputValueDefinition {
            description: None,
            name: SOURCE_HEADERS_ARGUMENT_NAME.clone(),
            ty: Type::List(Box::new(Type::NonNullNamed(
                http_header_mapping.name.clone(),
            )))
            .into(),
            default_value: None,
            directives: Default::default(),
        },
    ];

    let mut source_http_fields = IndexMap::new();
    for field in source_http_field_list {
        source_http_fields.insert(field.name.clone(), Component::new(field));
    }

    // input SourceHTTP {
    //   baseURL: String!
    //   headers: [HTTPHeaderMapping!]
    // }
    let source_http_spec = InputObjectType {
        name: link.type_name_in_schema(&SOURCE_HTTP_NAME_IN_SPEC),
        description: None,
        directives: Default::default(),
        fields: source_http_fields,
    };

    let source_http_pos = InputObjectTypeDefinitionPosition {
        type_name: source_http_spec.name.clone(),
    };

    // -------------------------------------------------------------------------

    // directive @source(
    //   name: String!
    //   http: SourceHTTP
    // ) repeatable on SCHEMA
    let source_spec = DirectiveSpecification::new(
        link.directive_name_in_schema(&SOURCE_DIRECTIVE_NAME_IN_SPEC),
        &[
            DirectiveArgumentSpecification {
                base_spec: ArgumentSpecification {
                    name: SOURCE_NAME_ARGUMENT_NAME.clone(),
                    get_type: |_| Ok(ty!(String!)),
                    default_value: None,
                },
                composition_strategy: None,
            },
            DirectiveArgumentSpecification {
                base_spec: ArgumentSpecification {
                    name: SOURCE_HTTP_ARGUMENT_NAME.clone(),
                    get_type: |s| {
                        let name = s
                            .metadata()
                            .ok_or_else(|| internal("missing metadata"))?
                            .for_identity(&ConnectSpecDefinition::identity())
                            .ok_or_else(|| internal("missing connect spec"))?
                            .type_name_in_schema(&SOURCE_HTTP_NAME_IN_SPEC);
                        Ok(Type::Named(name))
                    },
                    default_value: None,
                },
                composition_strategy: None,
            },
        ],
        true,
        &[DirectiveLocation::Schema],
        false,
        None,
    );

    json_selection_spec.check_or_add(schema)?;
    url_path_template_spec.check_or_add(schema)?;
    http_header_mapping_pos.pre_insert(schema)?;
    http_header_mapping_pos.insert(schema, http_header_mapping.into())?;

    connect_http_pos.pre_insert(schema)?;
    connect_http_pos.insert(schema, connect_http.into())?;
    connect_spec.check_or_add(schema)?;

    source_http_pos.pre_insert(schema)?;
    source_http_pos.insert(schema, source_http_spec.into())?;
    source_spec.check_or_add(schema)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use apollo_compiler::Schema;
    use insta::assert_snapshot;

    use super::check_or_add;
    use crate::schema::FederationSchema;
    use crate::sources::connect::spec::ConnectSpecDefinition;

    #[test]
    fn test() {
        let schema = Schema::parse(r#"
        type Query { hello: String }
        extend schema
          @link(url: "https://specs.apollo.dev/link/v1.0")
          @link(url: "https://specs.apollo.dev/connect/v0.1", import: ["@source"])
        directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA
        enum link__Purpose { SECURITY EXECUTION }
        scalar link__Import
        "#, "schema.graphql").unwrap();

        let mut federation_schema = FederationSchema::new(schema).unwrap();
        let link = federation_schema
            .metadata()
            .unwrap()
            .for_identity(&ConnectSpecDefinition::identity())
            .unwrap();

        check_or_add(&link, &mut federation_schema).unwrap();

        assert_snapshot!(federation_schema.schema().serialize().to_string(), @r###"
        schema {
          query: Query
        }

        extend schema @link(url: "https://specs.apollo.dev/link/v1.0") @link(url: "https://specs.apollo.dev/connect/v0.1", import: ["@source"])

        directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

        directive @connect(source: String, http: connect__ConnectHTTP, selection: connect__JSONSelection!, entity: Boolean = false) repeatable on FIELD_DEFINITION

        directive @source(name: String!, http: connect__SourceHTTP) repeatable on SCHEMA

        type Query {
          hello: String
        }

        enum link__Purpose {
          SECURITY
          EXECUTION
        }

        scalar link__Import

        scalar connect__JSONSelection

        scalar connect__URLPathTemplate

        input connect__HTTPHeaderMapping {
          name: String!
          as: String
          value: [String!]
        }

        input connect__ConnectHTTP {
          GET: connect__URLPathTemplate
          POST: connect__URLPathTemplate
          PUT: connect__URLPathTemplate
          PATCH: connect__URLPathTemplate
          DELETE: connect__URLPathTemplate
          body: connect__JSONSelection
          headers: [connect__HTTPHeaderMapping!]
        }

        input connect__SourceHTTP {
          baseURL: String!
          headers: [connect__HTTPHeaderMapping!]
        }
        "###);
    }
}
