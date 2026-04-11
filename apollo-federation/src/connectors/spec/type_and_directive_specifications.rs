//! Code for adding required definitions to a schema.
//! For example, directive definitions and the custom scalars they need.

use std::sync::Arc;

use apollo_compiler::Name;
use apollo_compiler::ast::DirectiveLocation;
use apollo_compiler::ast::Type;
use apollo_compiler::ast::Value;
use apollo_compiler::name;
use apollo_compiler::ty;

use super::connect::BATCH_ARGUMENT_NAME;
use super::connect::CONNECT_BATCH_NAME_IN_SPEC;
use super::connect::CONNECT_BODY_ARGUMENT_NAME;
use super::connect::CONNECT_DIRECTIVE_NAME_IN_SPEC;
use super::connect::CONNECT_ENTITY_ARGUMENT_NAME;
use super::connect::CONNECT_HTTP_NAME_IN_SPEC;
use super::connect::CONNECT_ID_ARGUMENT_NAME;
use super::connect::CONNECT_SELECTION_ARGUMENT_NAME;
use super::connect::CONNECT_SOURCE_ARGUMENT_NAME;
use super::connect::IS_SUCCESS_ARGUMENT_NAME;
use super::errors::ERRORS_ARGUMENT_NAME;
use super::errors::ERRORS_NAME_IN_SPEC;
use super::http::HEADERS_ARGUMENT_NAME;
use super::http::HTTP_ARGUMENT_NAME;
use super::http::HTTP_HEADER_MAPPING_FROM_ARGUMENT_NAME;
use super::http::HTTP_HEADER_MAPPING_NAME_ARGUMENT_NAME;
use super::http::HTTP_HEADER_MAPPING_NAME_IN_SPEC;
use super::http::HTTP_HEADER_MAPPING_VALUE_ARGUMENT_NAME;
use super::http::PATH_ARGUMENT_NAME;
use super::http::QUERY_PARAMS_ARGUMENT_NAME;
use super::http::URL_PATH_TEMPLATE_SCALAR_NAME;
use super::source::BaseUrl;
use super::source::SOURCE_DIRECTIVE_NAME_IN_SPEC;
use super::source::SOURCE_HTTP_NAME_IN_SPEC;
use super::source::SOURCE_NAME_ARGUMENT_NAME;
use crate::bail;
use crate::error::FederationError;
use crate::link::Link;
use crate::schema::FederationSchema;
use crate::schema::position::TypeDefinitionPosition;
use crate::schema::type_and_directive_specification::ArgumentSpecification;
use crate::schema::type_and_directive_specification::DirectiveArgumentSpecification;
use crate::schema::type_and_directive_specification::DirectiveSpecification;
use crate::schema::type_and_directive_specification::InputObjectTypeSpecification;
use crate::schema::type_and_directive_specification::ScalarTypeSpecification;
use crate::schema::type_and_directive_specification::TypeAndDirectiveSpecification;

pub(crate) const JSON_SELECTION_SCALAR_NAME: Name = name!("JSONSelection");

pub(crate) fn type_specifications() -> Vec<Box<dyn TypeAndDirectiveSpecification>> {
    vec![
        Box::new(json_selection_spec()),
        Box::new(url_path_template_spec()),
        Box::new(connector_errors_spec()),
        Box::new(http_header_mapping_spec()),
        Box::new(connect_batch_spec()),
        Box::new(source_http_spec()),
        Box::new(connect_http_spec()),
    ]
}

// scalar JSONSelection
fn json_selection_spec() -> ScalarTypeSpecification {
    ScalarTypeSpecification {
        name: JSON_SELECTION_SCALAR_NAME,
    }
}

// scalar URLPathTemplate
fn url_path_template_spec() -> ScalarTypeSpecification {
    ScalarTypeSpecification {
        name: URL_PATH_TEMPLATE_SCALAR_NAME,
    }
}

// input ConnectorErrors {
//   message: JSONSelection
//   extensions: JSONSelection
// }
fn connector_errors_spec() -> InputObjectTypeSpecification {
    InputObjectTypeSpecification {
        name: ERRORS_NAME_IN_SPEC,
        fields: |_| {
            Vec::from_iter([
                ArgumentSpecification {
                    name: name!(message),
                    get_type: |schema, link| {
                        let json_selection_name =
                            lookup_scalar_in_schema(&JSON_SELECTION_SCALAR_NAME, schema, link)?;
                        Ok(Type::Named(json_selection_name))
                    },
                    default_value: Default::default(),
                },
                ArgumentSpecification {
                    name: name!(extensions),
                    get_type: |schema, link| {
                        let json_selection_name =
                            lookup_scalar_in_schema(&JSON_SELECTION_SCALAR_NAME, schema, link)?;
                        Ok(Type::Named(json_selection_name))
                    },
                    default_value: Default::default(),
                },
            ])
        },
    }
}

// input HTTPHeaderMapping {
//   name: String!
//   from: String
//   value: String
// }
fn http_header_mapping_spec() -> InputObjectTypeSpecification {
    InputObjectTypeSpecification {
        name: HTTP_HEADER_MAPPING_NAME_IN_SPEC,
        fields: |_| {
            Vec::from_iter([
                ArgumentSpecification {
                    name: HTTP_HEADER_MAPPING_NAME_ARGUMENT_NAME,
                    get_type: |_, _| Ok(ty!(String!)),
                    default_value: Default::default(),
                },
                ArgumentSpecification {
                    name: HTTP_HEADER_MAPPING_FROM_ARGUMENT_NAME,
                    get_type: |_, _| Ok(ty!(String)),
                    default_value: Default::default(),
                },
                ArgumentSpecification {
                    name: HTTP_HEADER_MAPPING_VALUE_ARGUMENT_NAME,
                    get_type: |_, _| Ok(ty!(String)),
                    default_value: Default::default(),
                },
            ])
        },
    }
}

// input ConnectBatch {
//   maxSize: Int
// }
fn connect_batch_spec() -> InputObjectTypeSpecification {
    InputObjectTypeSpecification {
        name: CONNECT_BATCH_NAME_IN_SPEC,
        fields: |_| {
            Vec::from_iter([ArgumentSpecification {
                name: name!(maxSize),
                get_type: |_, _| Ok(ty!(Int)),
                default_value: Default::default(),
            }])
        },
    }
}

// input SourceHTTP {
//   baseURL: String!
//   headers: [HTTPHeaderMapping!]
//
//   # added in v0.2
//   path: JSONSelection
//   queryParams: JSONSelection
// }
fn source_http_spec() -> InputObjectTypeSpecification {
    InputObjectTypeSpecification {
        name: SOURCE_HTTP_NAME_IN_SPEC,
        fields: |_| {
            Vec::from_iter([
                ArgumentSpecification {
                    name: BaseUrl::ARGUMENT,
                    get_type: |_, _| Ok(ty!(String!)),
                    default_value: Default::default(),
                },
                ArgumentSpecification {
                    name: HEADERS_ARGUMENT_NAME,
                    get_type: |schema, link| {
                        let http_header_mapping = lookup_input_object_in_schema(
                            &HTTP_HEADER_MAPPING_NAME_IN_SPEC,
                            schema,
                            link,
                        )?;
                        Ok(Type::List(Box::new(Type::NonNullNamed(
                            http_header_mapping,
                        ))))
                    },
                    default_value: Default::default(),
                },
                ArgumentSpecification {
                    name: PATH_ARGUMENT_NAME,
                    get_type: |schema, link| {
                        let json_scalar_name =
                            lookup_scalar_in_schema(&JSON_SELECTION_SCALAR_NAME, schema, link)?;
                        Ok(Type::Named(json_scalar_name))
                    },
                    default_value: Default::default(),
                },
                ArgumentSpecification {
                    name: QUERY_PARAMS_ARGUMENT_NAME,
                    get_type: |schema, link| {
                        let json_scalar_name =
                            lookup_scalar_in_schema(&JSON_SELECTION_SCALAR_NAME, schema, link)?;
                        Ok(Type::Named(json_scalar_name))
                    },
                    default_value: Default::default(),
                },
            ])
        },
    }
}

// input ConnectHTTP {
//   GET: URLPathTemplate
//   POST: URLPathTemplate
//   PUT: URLPathTemplate
//   PATCH: URLPathTemplate
//   DELETE: URLPathTemplate
//   body: JSONSelection
//   headers: [HTTPHeaderMapping!]
//
//   # added in v0.2
//   path: JSONSelection
//   queryParams: JSONSelection
// }
fn connect_http_spec() -> InputObjectTypeSpecification {
    InputObjectTypeSpecification {
        name: CONNECT_HTTP_NAME_IN_SPEC,
        fields: |_| {
            Vec::from_iter([
                ArgumentSpecification {
                    name: name!(GET),
                    get_type: |schema, link| {
                        let url_path_template_scalar =
                            lookup_scalar_in_schema(&URL_PATH_TEMPLATE_SCALAR_NAME, schema, link)?;
                        Ok(Type::Named(url_path_template_scalar))
                    },
                    default_value: Default::default(),
                },
                ArgumentSpecification {
                    name: name!(POST),
                    get_type: |schema, link| {
                        let url_path_template_scalar =
                            lookup_scalar_in_schema(&URL_PATH_TEMPLATE_SCALAR_NAME, schema, link)?;
                        Ok(Type::Named(url_path_template_scalar))
                    },
                    default_value: Default::default(),
                },
                ArgumentSpecification {
                    name: name!(PUT),
                    get_type: |schema, link| {
                        let url_path_template_scalar =
                            lookup_scalar_in_schema(&URL_PATH_TEMPLATE_SCALAR_NAME, schema, link)?;
                        Ok(Type::Named(url_path_template_scalar))
                    },
                    default_value: Default::default(),
                },
                ArgumentSpecification {
                    name: name!(PATCH),
                    get_type: |schema, link| {
                        let url_path_template_scalar =
                            lookup_scalar_in_schema(&URL_PATH_TEMPLATE_SCALAR_NAME, schema, link)?;
                        Ok(Type::Named(url_path_template_scalar))
                    },
                    default_value: Default::default(),
                },
                ArgumentSpecification {
                    name: name!(DELETE),
                    get_type: |schema, link| {
                        let url_path_template_scalar =
                            lookup_scalar_in_schema(&URL_PATH_TEMPLATE_SCALAR_NAME, schema, link)?;
                        Ok(Type::Named(url_path_template_scalar))
                    },
                    default_value: Default::default(),
                },
                ArgumentSpecification {
                    name: CONNECT_BODY_ARGUMENT_NAME,
                    get_type: |schema, link| {
                        let json_selection_scalar =
                            lookup_scalar_in_schema(&JSON_SELECTION_SCALAR_NAME, schema, link)?;
                        Ok(Type::Named(json_selection_scalar))
                    },
                    default_value: Default::default(),
                },
                ArgumentSpecification {
                    name: HEADERS_ARGUMENT_NAME,
                    get_type: |schema, link| {
                        let http_header_mapping = lookup_input_object_in_schema(
                            &HTTP_HEADER_MAPPING_NAME_IN_SPEC,
                            schema,
                            link,
                        )?;
                        Ok(Type::List(Box::new(Type::NonNullNamed(
                            http_header_mapping,
                        ))))
                    },
                    default_value: Default::default(),
                },
                ArgumentSpecification {
                    name: PATH_ARGUMENT_NAME,
                    get_type: |schema, link| {
                        let json_selection_scalar =
                            lookup_scalar_in_schema(&JSON_SELECTION_SCALAR_NAME, schema, link)?;
                        Ok(Type::Named(json_selection_scalar))
                    },
                    default_value: Default::default(),
                },
                ArgumentSpecification {
                    name: QUERY_PARAMS_ARGUMENT_NAME,
                    get_type: |schema, link| {
                        let json_selection_scalar =
                            lookup_scalar_in_schema(&JSON_SELECTION_SCALAR_NAME, schema, link)?;
                        Ok(Type::Named(json_selection_scalar))
                    },
                    default_value: Default::default(),
                },
            ])
        },
    }
}

pub(crate) fn directive_specifications() -> Vec<Box<dyn TypeAndDirectiveSpecification>> {
    vec![
        Box::new(connect_directive_spec()),
        Box::new(source_directive_spec()),
    ]
}

// connect/v0.1:
// directive @connect(
//   source: String
//   http: ConnectHTTP
//   selection: JSONSelection!
//   entity: Boolean = false
// ) repeatable on FIELD_DEFINITION
//
// connect/v0.2:
// directive @connect(
//   source: String
//   id: String
//   http: ConnectHTTP!
//   batch: ConnectBatch
//   errors: ConnectorErrors
//   selection: JSONSelection!
//   entity: Boolean = false
//   isSuccess: JSONSelection
// ) repeatable on FIELD_DEFINITION | OBJECT
fn connect_directive_spec() -> DirectiveSpecification {
    DirectiveSpecification::new(
        CONNECT_DIRECTIVE_NAME_IN_SPEC,
        &[
            DirectiveArgumentSpecification {
                base_spec: ArgumentSpecification {
                    name: CONNECT_SOURCE_ARGUMENT_NAME,
                    get_type: |_, _| Ok(ty!(String)),
                    default_value: None,
                },
                composition_strategy: None,
            },
            DirectiveArgumentSpecification {
                composition_strategy: None,
                base_spec: ArgumentSpecification {
                    name: CONNECT_ID_ARGUMENT_NAME,
                    get_type: |_, _| Ok(ty!(String)),
                    default_value: None,
                },
            },
            DirectiveArgumentSpecification {
                base_spec: ArgumentSpecification {
                    name: HTTP_ARGUMENT_NAME,
                    get_type: |schema, link| {
                        let connect_http_input = lookup_input_object_in_schema(
                            &CONNECT_HTTP_NAME_IN_SPEC,
                            schema,
                            link,
                        )?;
                        Ok(Type::Named(connect_http_input))
                    },
                    default_value: None,
                },
                composition_strategy: None,
            },
            DirectiveArgumentSpecification {
                base_spec: ArgumentSpecification {
                    name: BATCH_ARGUMENT_NAME,
                    get_type: |schema, link| {
                        let batch_argument_input = lookup_input_object_in_schema(
                            &CONNECT_BATCH_NAME_IN_SPEC,
                            schema,
                            link,
                        )?;
                        Ok(Type::Named(batch_argument_input))
                    },
                    default_value: None,
                },
                composition_strategy: None,
            },
            DirectiveArgumentSpecification {
                base_spec: ArgumentSpecification {
                    name: ERRORS_ARGUMENT_NAME,
                    get_type: |schema, link| {
                        let connector_errors_input =
                            lookup_input_object_in_schema(&ERRORS_NAME_IN_SPEC, schema, link)?;
                        Ok(Type::Named(connector_errors_input))
                    },
                    default_value: None,
                },
                composition_strategy: None,
            },
            DirectiveArgumentSpecification {
                base_spec: ArgumentSpecification {
                    name: CONNECT_SELECTION_ARGUMENT_NAME,
                    get_type: |schema, link| {
                        let json_selection_scalar =
                            lookup_scalar_in_schema(&JSON_SELECTION_SCALAR_NAME, schema, link)?;
                        Ok(Type::NonNullNamed(json_selection_scalar))
                    },
                    default_value: None,
                },
                composition_strategy: None,
            },
            DirectiveArgumentSpecification {
                base_spec: ArgumentSpecification {
                    name: CONNECT_ENTITY_ARGUMENT_NAME,
                    get_type: |_, _| Ok(ty!(Boolean)),
                    default_value: Some(Value::Boolean(false)),
                },
                composition_strategy: None,
            },
            DirectiveArgumentSpecification {
                base_spec: ArgumentSpecification {
                    name: IS_SUCCESS_ARGUMENT_NAME,
                    get_type: |schema, link| {
                        let json_selection_scalar =
                            lookup_scalar_in_schema(&JSON_SELECTION_SCALAR_NAME, schema, link)?;
                        Ok(Type::Named(json_selection_scalar))
                    },
                    default_value: None,
                },
                composition_strategy: None,
            },
        ],
        true,
        &[
            DirectiveLocation::FieldDefinition,
            DirectiveLocation::Object,
        ],
        // TODO connectors use custom logic to merge them using join_directive
        //  we should update this logic to a more generic mechanism introduced with @cacheTag
        None,
        // Some(DirectiveCompositionOptions {
        //     supergraph_specification: &|v| {
        //         CONNECT_VERSIONS.get_dyn_minimum_required_version(v)
        //     },
        //     static_argument_transform: None,
        //     use_join_directive: true,
        // }),
    )
}

// directive @source(
//   name: String!
//   http: SourceHTTP!
//   errors: ConnectorErrors
//   isSuccess: JSONSelection
// ) repeatable on SCHEMA
fn source_directive_spec() -> DirectiveSpecification {
    DirectiveSpecification::new(
        SOURCE_DIRECTIVE_NAME_IN_SPEC,
        &[
            DirectiveArgumentSpecification {
                base_spec: ArgumentSpecification {
                    name: SOURCE_NAME_ARGUMENT_NAME,
                    get_type: |_, _| Ok(ty!(String!)),
                    default_value: None,
                },
                composition_strategy: None,
            },
            DirectiveArgumentSpecification {
                base_spec: ArgumentSpecification {
                    name: HTTP_ARGUMENT_NAME,
                    get_type: |schema, link| {
                        let source_http_input =
                            lookup_input_object_in_schema(&SOURCE_HTTP_NAME_IN_SPEC, schema, link)?;
                        Ok(Type::NonNullNamed(source_http_input))
                    },
                    default_value: None,
                },
                composition_strategy: None,
            },
            DirectiveArgumentSpecification {
                base_spec: ArgumentSpecification {
                    name: ERRORS_ARGUMENT_NAME,
                    get_type: |schema, link| {
                        let connector_errors_input =
                            lookup_input_object_in_schema(&ERRORS_NAME_IN_SPEC, schema, link)?;
                        Ok(Type::Named(connector_errors_input))
                    },
                    default_value: None,
                },
                composition_strategy: None,
            },
            DirectiveArgumentSpecification {
                base_spec: ArgumentSpecification {
                    name: IS_SUCCESS_ARGUMENT_NAME,
                    get_type: |schema, link| {
                        let json_selection_scalar =
                            lookup_scalar_in_schema(&JSON_SELECTION_SCALAR_NAME, schema, link)?;
                        Ok(Type::Named(json_selection_scalar))
                    },
                    default_value: None,
                },
                composition_strategy: None,
            },
        ],
        true,
        &[DirectiveLocation::Schema],
        None,
    )
}

fn lookup_feature_type_in_schema(
    name: &Name,
    schema: &FederationSchema,
    link: Option<&Arc<Link>>,
) -> Result<TypeDefinitionPosition, FederationError> {
    let Some(link) = link else {
        bail!("Type {name} shouldn't be added without being attached to a @connect spec")
    };
    let actual_name = link.type_name_in_schema(name);
    schema.get_type(actual_name)
}

fn lookup_scalar_in_schema(
    name: &Name,
    schema: &FederationSchema,
    link: Option<&Arc<Link>>,
) -> Result<Name, FederationError> {
    let actual_type = lookup_feature_type_in_schema(name, schema, link)?;
    if !matches!(actual_type, TypeDefinitionPosition::Scalar(_)) {
        bail!("Expected type {name} to be Scalar but it was {actual_type}")
    }
    Ok(actual_type.type_name().clone())
}

fn lookup_input_object_in_schema(
    name: &Name,
    schema: &FederationSchema,
    link: Option<&Arc<Link>>,
) -> Result<Name, FederationError> {
    let actual_type = lookup_feature_type_in_schema(name, schema, link)?;
    if !matches!(actual_type, TypeDefinitionPosition::InputObject(_)) {
        bail!("Expected type {name} to be InputObject but it was {actual_type}")
    }
    Ok(actual_type.type_name().clone())
}

#[cfg(test)]
mod tests {
    use insta::assert_snapshot;

    use crate::subgraph::typestate::Subgraph;

    #[test]
    fn expands_connect_v01() {
        let sdl = r#"
            type Query { hello: String }
            extend schema
              @link(url: "https://specs.apollo.dev/federation/v2.10")
              @link(url: "https://specs.apollo.dev/connect/v0.1", import: ["@source"])
        "#;

        let subgraph = Subgraph::parse("s1", "http://s1", sdl)
            .expect("parsed successfully")
            .expand_links()
            .expect("expanded successfully");
        assert_snapshot!(subgraph.schema_string(), @r#"
        schema {
          query: Query
        }

        extend schema @link(url: "https://specs.apollo.dev/link/v1.0") @link(url: "https://specs.apollo.dev/federation/v2.10") @link(url: "https://specs.apollo.dev/connect/v0.1", import: ["@source"])

        directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

        directive @federation__key(fields: federation__FieldSet!, resolvable: Boolean = true) repeatable on OBJECT | INTERFACE

        directive @federation__requires(fields: federation__FieldSet!) on FIELD_DEFINITION

        directive @federation__provides(fields: federation__FieldSet!) on FIELD_DEFINITION

        directive @federation__external(reason: String) on OBJECT | FIELD_DEFINITION

        directive @federation__tag(name: String!) repeatable on FIELD_DEFINITION | OBJECT | INTERFACE | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION | SCHEMA

        directive @federation__extends on OBJECT | INTERFACE

        directive @federation__shareable repeatable on OBJECT | FIELD_DEFINITION

        directive @federation__inaccessible on FIELD_DEFINITION | OBJECT | INTERFACE | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION

        directive @federation__override(from: String!, label: String) on FIELD_DEFINITION

        directive @federation__composeDirective(name: String) repeatable on SCHEMA

        directive @federation__interfaceObject on OBJECT

        directive @federation__authenticated on FIELD_DEFINITION | OBJECT | INTERFACE | SCALAR | ENUM

        directive @federation__requiresScopes(scopes: [[federation__Scope!]!]!) on FIELD_DEFINITION | OBJECT | INTERFACE | SCALAR | ENUM

        directive @federation__policy(policies: [[federation__Policy!]!]!) on FIELD_DEFINITION | OBJECT | INTERFACE | SCALAR | ENUM

        directive @federation__context(name: String!) repeatable on INTERFACE | OBJECT | UNION

        directive @federation__fromContext(field: federation__ContextFieldValue) on ARGUMENT_DEFINITION

        directive @federation__cost(weight: Int!) on ARGUMENT_DEFINITION | ENUM | FIELD_DEFINITION | INPUT_FIELD_DEFINITION | OBJECT | SCALAR

        directive @federation__listSize(assumedSize: Int, slicingArguments: [String!], sizedFields: [String!], requireOneSlicingArgument: Boolean = true) on FIELD_DEFINITION

        directive @connect(source: String, id: String, http: connect__ConnectHTTP, batch: connect__ConnectBatch, errors: connect__ConnectorErrors, selection: connect__JSONSelection!, entity: Boolean = false, isSuccess: connect__JSONSelection) repeatable on FIELD_DEFINITION | OBJECT

        directive @source(name: String!, http: connect__SourceHTTP!, errors: connect__ConnectorErrors, isSuccess: connect__JSONSelection) repeatable on SCHEMA

        type Query {
          hello: String
          _service: _Service!
        }

        enum link__Purpose {
          """
          `SECURITY` features provide metadata necessary to securely resolve fields.
          """
          SECURITY
          """
          `EXECUTION` features provide metadata necessary for operation execution.
          """
          EXECUTION
        }

        scalar link__Import

        scalar federation__FieldSet

        scalar federation__Scope

        scalar federation__Policy

        scalar federation__ContextFieldValue

        scalar connect__JSONSelection

        scalar connect__URLTemplate

        input connect__ConnectorErrors {
          message: connect__JSONSelection
          extensions: connect__JSONSelection
        }

        input connect__HTTPHeaderMapping {
          name: String!
          from: String
          value: String
        }

        input connect__ConnectBatch {
          maxSize: Int
        }

        input connect__SourceHTTP {
          baseURL: String!
          headers: [connect__HTTPHeaderMapping!]
          path: connect__JSONSelection
          queryParams: connect__JSONSelection
        }

        input connect__ConnectHTTP {
          GET: connect__URLTemplate
          POST: connect__URLTemplate
          PUT: connect__URLTemplate
          PATCH: connect__URLTemplate
          DELETE: connect__URLTemplate
          body: connect__JSONSelection
          headers: [connect__HTTPHeaderMapping!]
          path: connect__JSONSelection
          queryParams: connect__JSONSelection
        }

        scalar _Any

        type _Service {
          sdl: String
        }
        "#);
    }

    #[test]
    fn expands_connect_v0_2() {
        let sdl = r#"
        type Query { hello: String }
        extend schema
          @link(url: "https://specs.apollo.dev/federation/v2.11")
          @link(url: "https://specs.apollo.dev/connect/v0.2", import: ["@source"])
        "#;

        let subgraph = Subgraph::parse("s1", "http://s1", sdl)
            .expect("parsed successfully")
            .expand_links()
            .expect("expanded successfully");
        assert_snapshot!(subgraph.schema_string(), @r#"
        schema {
          query: Query
        }

        extend schema @link(url: "https://specs.apollo.dev/link/v1.0") @link(url: "https://specs.apollo.dev/federation/v2.11") @link(url: "https://specs.apollo.dev/connect/v0.2", import: ["@source"])

        directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

        directive @federation__key(fields: federation__FieldSet!, resolvable: Boolean = true) repeatable on OBJECT | INTERFACE

        directive @federation__requires(fields: federation__FieldSet!) on FIELD_DEFINITION

        directive @federation__provides(fields: federation__FieldSet!) on FIELD_DEFINITION

        directive @federation__external(reason: String) on OBJECT | FIELD_DEFINITION

        directive @federation__tag(name: String!) repeatable on FIELD_DEFINITION | OBJECT | INTERFACE | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION | SCHEMA

        directive @federation__extends on OBJECT | INTERFACE

        directive @federation__shareable repeatable on OBJECT | FIELD_DEFINITION

        directive @federation__inaccessible on FIELD_DEFINITION | OBJECT | INTERFACE | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION

        directive @federation__override(from: String!, label: String) on FIELD_DEFINITION

        directive @federation__composeDirective(name: String) repeatable on SCHEMA

        directive @federation__interfaceObject on OBJECT

        directive @federation__authenticated on FIELD_DEFINITION | OBJECT | INTERFACE | SCALAR | ENUM

        directive @federation__requiresScopes(scopes: [[federation__Scope!]!]!) on FIELD_DEFINITION | OBJECT | INTERFACE | SCALAR | ENUM

        directive @federation__policy(policies: [[federation__Policy!]!]!) on FIELD_DEFINITION | OBJECT | INTERFACE | SCALAR | ENUM

        directive @federation__context(name: String!) repeatable on INTERFACE | OBJECT | UNION

        directive @federation__fromContext(field: federation__ContextFieldValue) on ARGUMENT_DEFINITION

        directive @federation__cost(weight: Int!) on ARGUMENT_DEFINITION | ENUM | FIELD_DEFINITION | INPUT_FIELD_DEFINITION | OBJECT | SCALAR

        directive @federation__listSize(assumedSize: Int, slicingArguments: [String!], sizedFields: [String!], requireOneSlicingArgument: Boolean = true) on FIELD_DEFINITION

        directive @connect(source: String, id: String, http: connect__ConnectHTTP, batch: connect__ConnectBatch, errors: connect__ConnectorErrors, selection: connect__JSONSelection!, entity: Boolean = false, isSuccess: connect__JSONSelection) repeatable on FIELD_DEFINITION | OBJECT

        directive @source(name: String!, http: connect__SourceHTTP!, errors: connect__ConnectorErrors, isSuccess: connect__JSONSelection) repeatable on SCHEMA

        type Query {
          hello: String
          _service: _Service!
        }

        enum link__Purpose {
          """
          `SECURITY` features provide metadata necessary to securely resolve fields.
          """
          SECURITY
          """
          `EXECUTION` features provide metadata necessary for operation execution.
          """
          EXECUTION
        }

        scalar link__Import

        scalar federation__FieldSet

        scalar federation__Scope

        scalar federation__Policy

        scalar federation__ContextFieldValue

        scalar connect__JSONSelection

        scalar connect__URLTemplate

        input connect__ConnectorErrors {
          message: connect__JSONSelection
          extensions: connect__JSONSelection
        }

        input connect__HTTPHeaderMapping {
          name: String!
          from: String
          value: String
        }

        input connect__ConnectBatch {
          maxSize: Int
        }

        input connect__SourceHTTP {
          baseURL: String!
          headers: [connect__HTTPHeaderMapping!]
          path: connect__JSONSelection
          queryParams: connect__JSONSelection
        }

        input connect__ConnectHTTP {
          GET: connect__URLTemplate
          POST: connect__URLTemplate
          PUT: connect__URLTemplate
          PATCH: connect__URLTemplate
          DELETE: connect__URLTemplate
          body: connect__JSONSelection
          headers: [connect__HTTPHeaderMapping!]
          path: connect__JSONSelection
          queryParams: connect__JSONSelection
        }

        scalar _Any

        type _Service {
          sdl: String
        }
        "#);
    }

    #[test]
    fn expands_connect_v0_2_with_explicit_link_spec_no_federation() {
        let sdl = r#"
            type Query { hello: String }
            extend schema
              @link(url: "https://specs.apollo.dev/link/v1.0")
              @link(url: "https://specs.apollo.dev/connect/v0.2", import: ["@source"])
        "#;

        let subgraph = Subgraph::parse("s1", "http://s1", sdl)
            .expect("parsed successfully")
            .expand_links()
            .expect("expanded successfully");
        assert_snapshot!(subgraph.schema_string(), @r#"
        schema {
          query: Query
        }

        extend schema @link(url: "https://specs.apollo.dev/link/v1.0") @link(url: "https://specs.apollo.dev/connect/v0.2", import: ["@source"])

        directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

        directive @key(fields: _FieldSet!, resolvable: Boolean = true) repeatable on OBJECT | INTERFACE

        directive @requires(fields: _FieldSet!) on FIELD_DEFINITION

        directive @provides(fields: _FieldSet!) on FIELD_DEFINITION

        directive @external(reason: String) on OBJECT | FIELD_DEFINITION

        directive @tag(name: String!) repeatable on FIELD_DEFINITION | OBJECT | INTERFACE | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION

        directive @extends on OBJECT | INTERFACE

        directive @connect(source: String, id: String, http: connect__ConnectHTTP, batch: connect__ConnectBatch, errors: connect__ConnectorErrors, selection: connect__JSONSelection!, entity: Boolean = false, isSuccess: connect__JSONSelection) repeatable on FIELD_DEFINITION | OBJECT

        directive @source(name: String!, http: connect__SourceHTTP!, errors: connect__ConnectorErrors, isSuccess: connect__JSONSelection) repeatable on SCHEMA

        type Query {
          hello: String
          _service: _Service!
        }

        enum link__Purpose {
          """
          `SECURITY` features provide metadata necessary to securely resolve fields.
          """
          SECURITY
          """
          `EXECUTION` features provide metadata necessary for operation execution.
          """
          EXECUTION
        }

        scalar link__Import

        scalar _FieldSet

        scalar connect__JSONSelection

        scalar connect__URLTemplate

        input connect__ConnectorErrors {
          message: connect__JSONSelection
          extensions: connect__JSONSelection
        }

        input connect__HTTPHeaderMapping {
          name: String!
          from: String
          value: String
        }

        input connect__ConnectBatch {
          maxSize: Int
        }

        input connect__SourceHTTP {
          baseURL: String!
          headers: [connect__HTTPHeaderMapping!]
          path: connect__JSONSelection
          queryParams: connect__JSONSelection
        }

        input connect__ConnectHTTP {
          GET: connect__URLTemplate
          POST: connect__URLTemplate
          PUT: connect__URLTemplate
          PATCH: connect__URLTemplate
          DELETE: connect__URLTemplate
          body: connect__JSONSelection
          headers: [connect__HTTPHeaderMapping!]
          path: connect__JSONSelection
          queryParams: connect__JSONSelection
        }

        scalar _Any

        type _Service {
          sdl: String
        }
        "#);
    }

    #[test]
    fn test_v0_2_renames() {
        let sdl = r#"
            type Query { hello: String }
            extend schema
              @link(url: "https://specs.apollo.dev/federation/v2.11")
              @link(url: "https://specs.apollo.dev/connect/v0.2", import: [
                { name: "@source" as: "@api" }
                { name: "JSONSelection" as: "Mapping" }
                { name: "ConnectorErrors" as: "ErrorMappings" }
                "ConnectHTTP"
              ])
        "#;

        let subgraph = Subgraph::parse("s1", "http://s1", sdl)
            .expect("parsed successfully")
            .expand_links()
            .expect("expanded successfully");
        assert_snapshot!(subgraph.schema_string(), @r#"
        schema {
          query: Query
        }

        extend schema @link(url: "https://specs.apollo.dev/link/v1.0") @link(url: "https://specs.apollo.dev/federation/v2.11") @link(url: "https://specs.apollo.dev/connect/v0.2", import: [{name: "@source", as: "@api"}, {name: "JSONSelection", as: "Mapping"}, {name: "ConnectorErrors", as: "ErrorMappings"}, "ConnectHTTP"])

        directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

        directive @federation__key(fields: federation__FieldSet!, resolvable: Boolean = true) repeatable on OBJECT | INTERFACE

        directive @federation__requires(fields: federation__FieldSet!) on FIELD_DEFINITION

        directive @federation__provides(fields: federation__FieldSet!) on FIELD_DEFINITION

        directive @federation__external(reason: String) on OBJECT | FIELD_DEFINITION

        directive @federation__tag(name: String!) repeatable on FIELD_DEFINITION | OBJECT | INTERFACE | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION | SCHEMA

        directive @federation__extends on OBJECT | INTERFACE

        directive @federation__shareable repeatable on OBJECT | FIELD_DEFINITION

        directive @federation__inaccessible on FIELD_DEFINITION | OBJECT | INTERFACE | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION

        directive @federation__override(from: String!, label: String) on FIELD_DEFINITION

        directive @federation__composeDirective(name: String) repeatable on SCHEMA

        directive @federation__interfaceObject on OBJECT

        directive @federation__authenticated on FIELD_DEFINITION | OBJECT | INTERFACE | SCALAR | ENUM

        directive @federation__requiresScopes(scopes: [[federation__Scope!]!]!) on FIELD_DEFINITION | OBJECT | INTERFACE | SCALAR | ENUM

        directive @federation__policy(policies: [[federation__Policy!]!]!) on FIELD_DEFINITION | OBJECT | INTERFACE | SCALAR | ENUM

        directive @federation__context(name: String!) repeatable on INTERFACE | OBJECT | UNION

        directive @federation__fromContext(field: federation__ContextFieldValue) on ARGUMENT_DEFINITION

        directive @federation__cost(weight: Int!) on ARGUMENT_DEFINITION | ENUM | FIELD_DEFINITION | INPUT_FIELD_DEFINITION | OBJECT | SCALAR

        directive @federation__listSize(assumedSize: Int, slicingArguments: [String!], sizedFields: [String!], requireOneSlicingArgument: Boolean = true) on FIELD_DEFINITION

        directive @connect(source: String, id: String, http: ConnectHTTP, batch: connect__ConnectBatch, errors: ErrorMappings, selection: Mapping!, entity: Boolean = false, isSuccess: Mapping) repeatable on FIELD_DEFINITION | OBJECT

        directive @api(name: String!, http: connect__SourceHTTP!, errors: ErrorMappings, isSuccess: Mapping) repeatable on SCHEMA

        type Query {
          hello: String
          _service: _Service!
        }

        enum link__Purpose {
          """
          `SECURITY` features provide metadata necessary to securely resolve fields.
          """
          SECURITY
          """
          `EXECUTION` features provide metadata necessary for operation execution.
          """
          EXECUTION
        }

        scalar link__Import

        scalar federation__FieldSet

        scalar federation__Scope

        scalar federation__Policy

        scalar federation__ContextFieldValue

        scalar Mapping

        scalar connect__URLTemplate

        input ErrorMappings {
          message: Mapping
          extensions: Mapping
        }

        input connect__HTTPHeaderMapping {
          name: String!
          from: String
          value: String
        }

        input connect__ConnectBatch {
          maxSize: Int
        }

        input connect__SourceHTTP {
          baseURL: String!
          headers: [connect__HTTPHeaderMapping!]
          path: Mapping
          queryParams: Mapping
        }

        input ConnectHTTP {
          GET: connect__URLTemplate
          POST: connect__URLTemplate
          PUT: connect__URLTemplate
          PATCH: connect__URLTemplate
          DELETE: connect__URLTemplate
          body: Mapping
          headers: [connect__HTTPHeaderMapping!]
          path: Mapping
          queryParams: Mapping
        }

        scalar _Any

        type _Service {
          sdl: String
        }
        "#);
    }

    #[test]
    fn test_v0_2_compatible_defs() {
        // @connect does not have all the fields and is only applicable on FIELD_DEFINITION
        let sdl = r#"
            type Query { hello: String }
            extend schema
              @link(url: "https://specs.apollo.dev/federation/v2.11")
              @link(url: "https://specs.apollo.dev/connect/v0.2")

            scalar connect__JSONSelection
            input connect__ConnectHTTP {
              GET: connect__URLTemplate
            }
            directive @connect(source: String, http: connect__ConnectHTTP!, selection: connect__JSONSelection!) repeatable on FIELD_DEFINITION
        "#;

        let subgraph = Subgraph::parse("s1", "http://s1", sdl)
            .expect("parsed successfully")
            .expand_links()
            .expect("expanded successfully");
        assert_snapshot!(subgraph.schema_string(), @r#"
        schema {
          query: Query
        }

        extend schema @link(url: "https://specs.apollo.dev/link/v1.0") @link(url: "https://specs.apollo.dev/federation/v2.11") @link(url: "https://specs.apollo.dev/connect/v0.2")

        directive @connect(source: String, http: connect__ConnectHTTP!, selection: connect__JSONSelection!) repeatable on FIELD_DEFINITION

        directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

        directive @federation__key(fields: federation__FieldSet!, resolvable: Boolean = true) repeatable on OBJECT | INTERFACE

        directive @federation__requires(fields: federation__FieldSet!) on FIELD_DEFINITION

        directive @federation__provides(fields: federation__FieldSet!) on FIELD_DEFINITION

        directive @federation__external(reason: String) on OBJECT | FIELD_DEFINITION

        directive @federation__tag(name: String!) repeatable on FIELD_DEFINITION | OBJECT | INTERFACE | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION | SCHEMA

        directive @federation__extends on OBJECT | INTERFACE

        directive @federation__shareable repeatable on OBJECT | FIELD_DEFINITION

        directive @federation__inaccessible on FIELD_DEFINITION | OBJECT | INTERFACE | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION

        directive @federation__override(from: String!, label: String) on FIELD_DEFINITION

        directive @federation__composeDirective(name: String) repeatable on SCHEMA

        directive @federation__interfaceObject on OBJECT

        directive @federation__authenticated on FIELD_DEFINITION | OBJECT | INTERFACE | SCALAR | ENUM

        directive @federation__requiresScopes(scopes: [[federation__Scope!]!]!) on FIELD_DEFINITION | OBJECT | INTERFACE | SCALAR | ENUM

        directive @federation__policy(policies: [[federation__Policy!]!]!) on FIELD_DEFINITION | OBJECT | INTERFACE | SCALAR | ENUM

        directive @federation__context(name: String!) repeatable on INTERFACE | OBJECT | UNION

        directive @federation__fromContext(field: federation__ContextFieldValue) on ARGUMENT_DEFINITION

        directive @federation__cost(weight: Int!) on ARGUMENT_DEFINITION | ENUM | FIELD_DEFINITION | INPUT_FIELD_DEFINITION | OBJECT | SCALAR

        directive @federation__listSize(assumedSize: Int, slicingArguments: [String!], sizedFields: [String!], requireOneSlicingArgument: Boolean = true) on FIELD_DEFINITION

        directive @connect__source(name: String!, http: connect__SourceHTTP!, errors: connect__ConnectorErrors, isSuccess: connect__JSONSelection) repeatable on SCHEMA

        type Query {
          hello: String
          _service: _Service!
        }

        scalar connect__JSONSelection

        input connect__ConnectHTTP {
          GET: connect__URLTemplate
        }

        enum link__Purpose {
          """
          `SECURITY` features provide metadata necessary to securely resolve fields.
          """
          SECURITY
          """
          `EXECUTION` features provide metadata necessary for operation execution.
          """
          EXECUTION
        }

        scalar link__Import

        scalar federation__FieldSet

        scalar federation__Scope

        scalar federation__Policy

        scalar federation__ContextFieldValue

        scalar connect__URLTemplate

        input connect__ConnectorErrors {
          message: connect__JSONSelection
          extensions: connect__JSONSelection
        }

        input connect__HTTPHeaderMapping {
          name: String!
          from: String
          value: String
        }

        input connect__ConnectBatch {
          maxSize: Int
        }

        input connect__SourceHTTP {
          baseURL: String!
          headers: [connect__HTTPHeaderMapping!]
          path: connect__JSONSelection
          queryParams: connect__JSONSelection
        }

        scalar _Any

        type _Service {
          sdl: String
        }
        "#);
    }

    #[test]
    fn test_v0_2_incompatible_defs() {
        // @connect entity is a Boolean but redefined as a String!
        let sdl = r#"
            type Query { hello: String }
            extend schema
              @link(url: "https://specs.apollo.dev/federation/v2.11")
              @link(url: "https://specs.apollo.dev/connect/v0.2")

            scalar connect__JSONSelection
            input connect__ConnectHTTP {
              GET: connect__URLTemplate
            }
            directive @connect(source: String, http: connect__ConnectHTTP!, selection: connect__JSONSelection!, entity: String!) repeatable on FIELD_DEFINITION
        "#;

        let subgraph_error = Subgraph::parse("s1", "http://s1", sdl)
            .expect("parsed successfully")
            .expand_links()
            .expect_err("should fail");
        assert_eq!(
            subgraph_error.to_string().trim(),
            r###"DIRECTIVE_DEFINITION_INVALID [s1] Invalid definition for directive "@connect": argument "entity" should have type "Boolean" but found type "String""###
        );
    }
}
