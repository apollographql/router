use apollo_compiler::{
    ast::{DirectiveLocation, Name, Type, Value},
    name, ty,
};

use crate::{
    error::FederationError,
    link::Link,
    schema::{
        type_and_directive_specification::{
            ArgumentSpecification, DirectiveArgumentSpecification, DirectiveSpecification,
            InputFieldSpecification, InputTypeSpecification, ScalarTypeSpecification,
            TypeAndDirectiveSpecification,
        },
        FederationSchema,
    },
    sources::connect::spec::schema::{
        CONNECT_BODY_ARGUMENT_NAME, CONNECT_HEADERS_ARGUMENT_NAME,
        HTTP_HEADER_MAPPING_AS_ARGUMENT_NAME, HTTP_HEADER_MAPPING_NAME_ARGUMENT_NAME,
        HTTP_HEADER_MAPPING_VALUE_ARGUMENT_NAME, SOURCE_BASE_URL_ARGUMENT_NAME,
        SOURCE_HEADERS_ARGUMENT_NAME,
    },
};

use super::{
    schema::{
        CONNECT_DIRECTIVE_NAME_IN_SPEC, CONNECT_ENTITY_ARGUMENT_NAME, CONNECT_HTTP_ARGUMENT_NAME,
        CONNECT_HTTP_NAME_IN_SPEC, CONNECT_SELECTION_ARGUMENT_NAME, CONNECT_SOURCE_ARGUMENT_NAME,
        HTTP_HEADER_MAPPING_NAME_IN_SPEC, JSON_SELECTION_SCALAR_NAME,
        SOURCE_DIRECTIVE_NAME_IN_SPEC, SOURCE_HTTP_ARGUMENT_NAME, SOURCE_HTTP_NAME_IN_SPEC,
        SOURCE_NAME_ARGUMENT_NAME, URL_PATH_TEMPLATE_SCALAR_NAME,
    },
    ConnectSpecDefinition,
};

pub(super) fn check_or_add(
    link: &Link,
    schema: &mut FederationSchema,
) -> Result<(), FederationError> {
    let json_selection_spec = ScalarTypeSpecification {
        name: link.type_name_in_schema(&JSON_SELECTION_SCALAR_NAME),
    };

    let url_path_template_spec = ScalarTypeSpecification {
        name: link.type_name_in_schema(&URL_PATH_TEMPLATE_SCALAR_NAME),
    };

    let http_header_mapping = InputTypeSpecification {
        name: link.type_name_in_schema(&HTTP_HEADER_MAPPING_NAME_IN_SPEC),
        fields: |_| {
            vec![
                InputFieldSpecification {
                    name: Name::new_unchecked(HTTP_HEADER_MAPPING_NAME_ARGUMENT_NAME.into()),
                    ty: ty!(String!),
                    default_value: None,
                },
                InputFieldSpecification {
                    name: Name::new_unchecked(HTTP_HEADER_MAPPING_AS_ARGUMENT_NAME.into()),
                    ty: ty!(String),
                    default_value: None,
                },
                InputFieldSpecification {
                    name: Name::new_unchecked(HTTP_HEADER_MAPPING_VALUE_ARGUMENT_NAME.into()),
                    ty: ty!([String!]),
                    default_value: None,
                },
            ]
        },
    };

    let connect_http_spec = InputTypeSpecification {
        name: link.type_name_in_schema(&CONNECT_HTTP_NAME_IN_SPEC),
        fields: |s| {
            let link = s
                .metadata()
                .unwrap()
                .for_identity(&ConnectSpecDefinition::identity())
                .unwrap();
            let url_path_template_name = link.type_name_in_schema(&URL_PATH_TEMPLATE_SCALAR_NAME);
            let json_selection_template_name =
                link.type_name_in_schema(&JSON_SELECTION_SCALAR_NAME);
            let http_header_mapping_name =
                link.type_name_in_schema(&HTTP_HEADER_MAPPING_NAME_IN_SPEC);
            vec![
                InputFieldSpecification {
                    name: name!(GET),
                    ty: Type::Named(url_path_template_name.clone()),
                    default_value: None,
                },
                InputFieldSpecification {
                    name: name!(POST),
                    ty: Type::Named(url_path_template_name.clone()),
                    default_value: None,
                },
                InputFieldSpecification {
                    name: name!(PUT),
                    ty: Type::Named(url_path_template_name.clone()),
                    default_value: None,
                },
                InputFieldSpecification {
                    name: name!(PATCH),
                    ty: Type::Named(url_path_template_name.clone()),
                    default_value: None,
                },
                InputFieldSpecification {
                    name: name!(DELETE),
                    ty: Type::Named(url_path_template_name.clone()),
                    default_value: None,
                },
                InputFieldSpecification {
                    name: Name::new_unchecked(CONNECT_BODY_ARGUMENT_NAME.into()),
                    ty: Type::Named(json_selection_template_name.clone()),
                    default_value: None,
                },
                InputFieldSpecification {
                    name: Name::new_unchecked(CONNECT_HEADERS_ARGUMENT_NAME.into()),
                    ty: Type::List(Box::new(Type::NonNullNamed(http_header_mapping_name))),
                    default_value: None,
                },
            ]
        },
    };

    let connect_spec = DirectiveSpecification::new(
        link.directive_name_in_schema(&CONNECT_DIRECTIVE_NAME_IN_SPEC),
        &[
            DirectiveArgumentSpecification {
                base_spec: ArgumentSpecification {
                    name: Name::new_unchecked(CONNECT_SOURCE_ARGUMENT_NAME.into()),
                    get_type: |_| Ok(ty!(String)),
                    default_value: None,
                },
                composition_strategy: None,
            },
            DirectiveArgumentSpecification {
                base_spec: ArgumentSpecification {
                    name: Name::new_unchecked(CONNECT_HTTP_ARGUMENT_NAME.into()),
                    get_type: |s| {
                        let name = s
                            .metadata()
                            .unwrap()
                            .for_identity(&ConnectSpecDefinition::identity())
                            .unwrap()
                            .type_name_in_schema(&CONNECT_HTTP_NAME_IN_SPEC);
                        Ok(Type::Named(name))
                    },
                    default_value: None,
                },
                composition_strategy: None,
            },
            DirectiveArgumentSpecification {
                base_spec: ArgumentSpecification {
                    name: Name::new_unchecked(CONNECT_SELECTION_ARGUMENT_NAME.into()),
                    get_type: |s| {
                        let name = s
                            .metadata()
                            .unwrap()
                            .for_identity(&ConnectSpecDefinition::identity())
                            .unwrap()
                            .type_name_in_schema(&JSON_SELECTION_SCALAR_NAME);
                        Ok(Type::NonNullNamed(name))
                    },
                    default_value: None,
                },
                composition_strategy: None,
            },
            DirectiveArgumentSpecification {
                base_spec: ArgumentSpecification {
                    name: Name::new_unchecked(CONNECT_ENTITY_ARGUMENT_NAME.into()),
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

    let source_http_spec = InputTypeSpecification {
        name: link.type_name_in_schema(&SOURCE_HTTP_NAME_IN_SPEC),
        fields: |s| {
            let link = s
                .metadata()
                .unwrap()
                .for_identity(&ConnectSpecDefinition::identity())
                .unwrap();
            let http_header_mapping_name =
                link.type_name_in_schema(&HTTP_HEADER_MAPPING_NAME_IN_SPEC);
            vec![
                InputFieldSpecification {
                    name: Name::new_unchecked(SOURCE_BASE_URL_ARGUMENT_NAME.into()),
                    ty: ty!(String!),
                    default_value: None,
                },
                InputFieldSpecification {
                    name: Name::new_unchecked(SOURCE_HEADERS_ARGUMENT_NAME.into()),
                    ty: Type::List(Box::new(Type::NonNullNamed(http_header_mapping_name))),
                    default_value: None,
                },
            ]
        },
    };

    let source_spec = DirectiveSpecification::new(
        link.directive_name_in_schema(&SOURCE_DIRECTIVE_NAME_IN_SPEC),
        &[
            DirectiveArgumentSpecification {
                base_spec: ArgumentSpecification {
                    name: Name::new_unchecked(SOURCE_NAME_ARGUMENT_NAME.into()),
                    get_type: |_| Ok(ty!(String!)),
                    default_value: None,
                },
                composition_strategy: None,
            },
            DirectiveArgumentSpecification {
                base_spec: ArgumentSpecification {
                    name: Name::new_unchecked(SOURCE_HTTP_ARGUMENT_NAME.into()),
                    get_type: |s| {
                        let name = s
                            .metadata()
                            .unwrap()
                            .for_identity(&ConnectSpecDefinition::identity())
                            .unwrap()
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
    http_header_mapping.check_or_add(schema)?;

    connect_http_spec.check_or_add(schema)?;
    connect_spec.check_or_add(schema)?;

    source_http_spec.check_or_add(schema)?;
    source_spec.check_or_add(schema)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use apollo_compiler::Schema;
    use insta::assert_snapshot;

    use crate::{schema::FederationSchema, sources::connect::spec::ConnectSpecDefinition};

    use super::check_or_add;

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
