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
use crate::connectors::spec::ConnectSpec;
use crate::connectors::spec::source::FRAGMENTS_NAME_ARGUMENT;
use crate::error::SingleFederationError;
use crate::link::Link;
use crate::schema::FederationSchema;
use crate::schema::type_and_directive_specification::ArgumentSpecification;
use crate::schema::type_and_directive_specification::DirectiveArgumentSpecification;
use crate::schema::type_and_directive_specification::DirectiveSpecification;
use crate::schema::type_and_directive_specification::InputObjectTypeSpecification;
use crate::schema::type_and_directive_specification::ScalarTypeSpecification;
use crate::schema::type_and_directive_specification::TypeAndDirectiveSpecification;

macro_rules! internal {
    ($s:expr) => {
        SingleFederationError::Internal {
            message: $s.to_string(),
        }
    };
}

fn link(s: &FederationSchema) -> Result<Arc<Link>, SingleFederationError> {
    s.metadata()
        .ok_or_else(|| internal!("missing metadata"))?
        .for_identity(&ConnectSpec::identity())
        .ok_or_else(|| internal!("missing connect spec"))
}

pub(crate) const JSON_SELECTION_SCALAR_NAME: Name = name!("JSONSelection");

fn json_selection_spec() -> ScalarTypeSpecification {
    ScalarTypeSpecification {
        name: JSON_SELECTION_SCALAR_NAME,
    }
}

fn url_path_template_spec() -> ScalarTypeSpecification {
    ScalarTypeSpecification {
        name: URL_PATH_TEMPLATE_SCALAR_NAME,
    }
}

// input HTTPHeaderMapping {
//   name: String!
//   as: String
//   value: [String!]
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
                    get_type: |_, _| Ok(ty!([String!])),
                    default_value: Default::default(),
                },
            ])
        },
    }
}

// input ConnectHTTP {
//   GET: URLTemplate
//   POST: URLTemplate
//   PUT: URLTemplate
//   PATCH: URLTemplate
//   DELETE: URLTemplate
//   body: JSONSelection
//   headers: [HTTPHeaderMapping!]
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
                    get_type: |s, _| {
                        let name = link(s)?.type_name_in_schema(&URL_PATH_TEMPLATE_SCALAR_NAME);
                        Ok(Type::Named(name))
                    },
                    default_value: Default::default(),
                },
                ArgumentSpecification {
                    name: name!(POST),
                    get_type: |s, _| {
                        let name = link(s)?.type_name_in_schema(&URL_PATH_TEMPLATE_SCALAR_NAME);
                        Ok(Type::Named(name))
                    },
                    default_value: Default::default(),
                },
                ArgumentSpecification {
                    name: name!(PUT),
                    get_type: |s, _| {
                        let name = link(s)?.type_name_in_schema(&URL_PATH_TEMPLATE_SCALAR_NAME);
                        Ok(Type::Named(name))
                    },
                    default_value: Default::default(),
                },
                ArgumentSpecification {
                    name: name!(PATCH),
                    get_type: |s, _| {
                        let name = link(s)?.type_name_in_schema(&URL_PATH_TEMPLATE_SCALAR_NAME);
                        Ok(Type::Named(name))
                    },
                    default_value: Default::default(),
                },
                ArgumentSpecification {
                    name: name!(DELETE),
                    get_type: |s, _| {
                        let name = link(s)?.type_name_in_schema(&URL_PATH_TEMPLATE_SCALAR_NAME);
                        Ok(Type::Named(name))
                    },
                    default_value: Default::default(),
                },
                ArgumentSpecification {
                    name: CONNECT_BODY_ARGUMENT_NAME,
                    get_type: |s, _| {
                        let name = link(s)?.type_name_in_schema(&JSON_SELECTION_SCALAR_NAME);
                        Ok(Type::Named(name))
                    },
                    default_value: Default::default(),
                },
                ArgumentSpecification {
                    name: HEADERS_ARGUMENT_NAME,
                    get_type: |s, _| {
                        let name = link(s)?.type_name_in_schema(&HTTP_HEADER_MAPPING_NAME_IN_SPEC);
                        Ok(Type::List(Box::new(Type::NonNullNamed(name))))
                    },
                    default_value: Default::default(),
                },
                ArgumentSpecification {
                    name: PATH_ARGUMENT_NAME,
                    get_type: |s, _| {
                        let name = link(s)?.type_name_in_schema(&JSON_SELECTION_SCALAR_NAME);
                        Ok(Type::Named(name))
                    },
                    default_value: Default::default(),
                },
                ArgumentSpecification {
                    name: QUERY_PARAMS_ARGUMENT_NAME,
                    get_type: |s, _| {
                        let name = link(s)?.type_name_in_schema(&JSON_SELECTION_SCALAR_NAME);
                        Ok(Type::Named(name))
                    },
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
                    get_type: |s, _| {
                        let name = link(s)?.type_name_in_schema(&JSON_SELECTION_SCALAR_NAME);
                        Ok(Type::Named(name))
                    },
                    default_value: Default::default(),
                },
                ArgumentSpecification {
                    name: name!(extensions),
                    get_type: |s, _| {
                        let name = link(s)?.type_name_in_schema(&JSON_SELECTION_SCALAR_NAME);
                        Ok(Type::Named(name))
                    },
                    default_value: Default::default(),
                },
            ])
        },
    }
}

// input SourceHTTP {
//   baseURL: String!
//   headers: [HTTPHeaderMapping!]
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
                    get_type: |s, _| {
                        let name = link(s)?.type_name_in_schema(&HTTP_HEADER_MAPPING_NAME_IN_SPEC);
                        Ok(Type::List(Box::new(Type::NonNullNamed(name))))
                    },
                    default_value: Default::default(),
                },
                ArgumentSpecification {
                    name: PATH_ARGUMENT_NAME,
                    get_type: |s, _| {
                        let name = link(s)?.type_name_in_schema(&JSON_SELECTION_SCALAR_NAME);
                        Ok(Type::Named(name))
                    },
                    default_value: Default::default(),
                },
                ArgumentSpecification {
                    name: QUERY_PARAMS_ARGUMENT_NAME,
                    get_type: |s, _| {
                        let name = link(s)?.type_name_in_schema(&JSON_SELECTION_SCALAR_NAME);
                        Ok(Type::Named(name))
                    },
                    default_value: Default::default(),
                },
            ])
        },
    }
}

pub(crate) fn type_specifications() -> Vec<Box<dyn TypeAndDirectiveSpecification>> {
    vec![
        Box::new(json_selection_spec()),
        Box::new(url_path_template_spec()),
        Box::new(http_header_mapping_spec()),
        Box::new(connect_http_spec()),
        Box::new(connect_batch_spec()),
        Box::new(connector_errors_spec()),
        Box::new(source_http_spec()),
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
//   id: String
//   source: String
//   http: ConnectHTTP
//   selection: JSONSelection!
//   entity: Boolean = false
//   batch: ConnectBatch
//   errors: ConnectErrors
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
                base_spec: ArgumentSpecification {
                    name: HTTP_ARGUMENT_NAME,
                    get_type: |s, _| {
                        let name = link(s)?.type_name_in_schema(&CONNECT_HTTP_NAME_IN_SPEC);
                        Ok(Type::Named(name))
                    },
                    default_value: None,
                },
                composition_strategy: None,
            },
            DirectiveArgumentSpecification {
                base_spec: ArgumentSpecification {
                    name: BATCH_ARGUMENT_NAME,
                    get_type: |s, _| {
                        let name = link(s)?.type_name_in_schema(&CONNECT_BATCH_NAME_IN_SPEC);
                        Ok(Type::Named(name))
                    },
                    default_value: None,
                },
                composition_strategy: None,
            },
            DirectiveArgumentSpecification {
                base_spec: ArgumentSpecification {
                    name: ERRORS_ARGUMENT_NAME,
                    get_type: |s, _| {
                        let name = link(s)?.type_name_in_schema(&ERRORS_NAME_IN_SPEC);
                        Ok(Type::Named(name))
                    },
                    default_value: None,
                },
                composition_strategy: None,
            },
            DirectiveArgumentSpecification {
                base_spec: ArgumentSpecification {
                    name: IS_SUCCESS_ARGUMENT_NAME,
                    get_type: |s, _| {
                        let name = link(s)?.type_name_in_schema(&JSON_SELECTION_SCALAR_NAME);
                        Ok(Type::Named(name))
                    },
                    default_value: None,
                },
                composition_strategy: None,
            },
            DirectiveArgumentSpecification {
                base_spec: ArgumentSpecification {
                    name: CONNECT_SELECTION_ARGUMENT_NAME,
                    get_type: |s, _| {
                        let name = link(s)?.type_name_in_schema(&JSON_SELECTION_SCALAR_NAME);
                        Ok(Type::NonNullNamed(name))
                    },
                    default_value: None,
                },
                composition_strategy: None,
            },
            DirectiveArgumentSpecification {
                base_spec: ArgumentSpecification {
                    name: CONNECT_ENTITY_ARGUMENT_NAME,
                    get_type: |_, _| Ok(Type::Named(name!(Boolean))),
                    default_value: Some(Value::Boolean(false)),
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
        ],
        true,
        &[
            DirectiveLocation::FieldDefinition,
            DirectiveLocation::Object,
        ],
        false,
        None,
        None,
    )
}

// directive @source(
//   name: String!
//   http: SourceHTTP
//   errors: ConnectorErrors
//   fragments: BTreeMap<Name, JsonSelection>
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
                    get_type: |s, _| {
                        let name = link(s)?.type_name_in_schema(&SOURCE_HTTP_NAME_IN_SPEC);
                        Ok(Type::Named(name))
                    },
                    default_value: None,
                },
                composition_strategy: None,
            },
            DirectiveArgumentSpecification {
                base_spec: ArgumentSpecification {
                    name: ERRORS_ARGUMENT_NAME,
                    get_type: |s, _| {
                        let name = link(s)?.type_name_in_schema(&ERRORS_NAME_IN_SPEC);
                        Ok(Type::Named(name))
                    },
                    default_value: None,
                },
                composition_strategy: None,
            },
            DirectiveArgumentSpecification {
                base_spec: ArgumentSpecification {
                    name: FRAGMENTS_NAME_ARGUMENT,
                    get_type: |s, _| {
                        let name = link(s)?.type_name_in_schema(&JSON_SELECTION_SCALAR_NAME);
                        Ok(Type::Named(name))
                    },
                    default_value: None,
                },
                composition_strategy: None,
            },
            DirectiveArgumentSpecification {
                base_spec: ArgumentSpecification {
                    name: IS_SUCCESS_ARGUMENT_NAME,
                    get_type: |s, _| {
                        let name = link(s)?.type_name_in_schema(&JSON_SELECTION_SCALAR_NAME);
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
        None,
    )
}

pub(crate) fn directive_specifications() -> Vec<Box<dyn TypeAndDirectiveSpecification>> {
    vec![
        Box::new(connect_directive_spec()),
        Box::new(source_directive_spec()),
    ]
}

#[cfg(test)]
mod tests {
    use apollo_compiler::Schema;
    use insta::assert_snapshot;

    use crate::connectors::spec::CONNECT_VERSIONS;
    use crate::connectors::spec::ConnectSpec;
    use crate::link::spec_definition::SpecDefinition;
    use crate::schema::FederationSchema;

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
            .for_identity(&ConnectSpec::identity())
            .unwrap();

        let spec = CONNECT_VERSIONS.find(&link.url.version).unwrap();
        spec.add_elements_to_schema(&mut federation_schema).unwrap();

        assert_snapshot!(federation_schema.schema().serialize().to_string(), @r#"
        schema {
          query: Query
        }

        extend schema @link(url: "https://specs.apollo.dev/link/v1.0") @link(url: "https://specs.apollo.dev/connect/v0.1", import: ["@source"])

        directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

        directive @connect(source: String, http: connect__ConnectHTTP, batch: connect__ConnectBatch, errors: connect__ConnectorErrors, isSuccess: connect__JSONSelection, selection: connect__JSONSelection!, entity: Boolean = false, id: String) repeatable on FIELD_DEFINITION | OBJECT

        directive @source(name: String!, http: connect__SourceHTTP, errors: connect__ConnectorErrors, isSuccess: connect__JSONSelection) repeatable on SCHEMA

        type Query {
          hello: String
        }

        enum link__Purpose {
          SECURITY
          EXECUTION
        }

        scalar link__Import

        scalar connect__JSONSelection

        scalar connect__URLTemplate

        input connect__HTTPHeaderMapping {
          name: String!
          from: String
          value: [String!]
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

        input connect__ConnectBatch {
          maxSize: Int
        }

        input connect__ConnectorErrors {
          message: connect__JSONSelection
          extensions: connect__JSONSelection
        }

        input connect__SourceHTTP {
          baseURL: String!
          headers: [connect__HTTPHeaderMapping!]
          path: connect__JSONSelection
          queryParams: connect__JSONSelection
        }
        "#);
    }

    #[test]
    fn test_v0_2() {
        let schema = Schema::parse(r#"
        type Query { hello: String }
        extend schema
          @link(url: "https://specs.apollo.dev/link/v1.0")
          @link(url: "https://specs.apollo.dev/connect/v0.2", import: ["@source"])
        directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA
        enum link__Purpose { SECURITY EXECUTION }
        scalar link__Import
        "#, "schema.graphql").unwrap();

        let mut federation_schema = FederationSchema::new(schema).unwrap();
        let link = federation_schema
            .metadata()
            .unwrap()
            .for_identity(&ConnectSpec::identity())
            .unwrap();

        let spec = CONNECT_VERSIONS.find(&link.url.version).unwrap();
        spec.add_elements_to_schema(&mut federation_schema).unwrap();

        assert_snapshot!(federation_schema.schema().serialize().to_string(), @r#"
        schema {
          query: Query
        }

        extend schema @link(url: "https://specs.apollo.dev/link/v1.0") @link(url: "https://specs.apollo.dev/connect/v0.2", import: ["@source"])

        directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

        directive @connect(source: String, http: connect__ConnectHTTP, batch: connect__ConnectBatch, errors: connect__ConnectorErrors, isSuccess: connect__JSONSelection, selection: connect__JSONSelection!, entity: Boolean = false, id: String) repeatable on FIELD_DEFINITION | OBJECT

        directive @source(name: String!, http: connect__SourceHTTP, errors: connect__ConnectorErrors, isSuccess: connect__JSONSelection) repeatable on SCHEMA

        type Query {
          hello: String
        }

        enum link__Purpose {
          SECURITY
          EXECUTION
        }

        scalar link__Import

        scalar connect__JSONSelection

        scalar connect__URLTemplate

        input connect__HTTPHeaderMapping {
          name: String!
          from: String
          value: [String!]
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

        input connect__ConnectBatch {
          maxSize: Int
        }

        input connect__ConnectorErrors {
          message: connect__JSONSelection
          extensions: connect__JSONSelection
        }

        input connect__SourceHTTP {
          baseURL: String!
          headers: [connect__HTTPHeaderMapping!]
          path: connect__JSONSelection
          queryParams: connect__JSONSelection
        }
        "#);
    }

    #[test]
    fn test_v0_2_renames() {
        let schema = Schema::parse(r#"
        type Query { hello: String }
        extend schema
          @link(url: "https://specs.apollo.dev/link/v1.0")
          @link(url: "https://specs.apollo.dev/connect/v0.2", import: [
            { name: "@source" as: "@api" }
            { name: "JSONSelection" as: "Mapping" }
            { name: "ConnectorErrors" as: "ErrorMappings" }
            "ConnectHTTP"
          ])
        directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA
        enum link__Purpose { SECURITY EXECUTION }
        scalar link__Import
        "#, "schema.graphql").unwrap();

        let mut federation_schema = FederationSchema::new(schema).unwrap();
        let link = federation_schema
            .metadata()
            .unwrap()
            .for_identity(&ConnectSpec::identity())
            .unwrap();

        let spec = CONNECT_VERSIONS.find(&link.url.version).unwrap();
        spec.add_elements_to_schema(&mut federation_schema).unwrap();

        assert_snapshot!(federation_schema.schema().serialize().to_string(), @r#"
        schema {
          query: Query
        }

        extend schema @link(url: "https://specs.apollo.dev/link/v1.0") @link(url: "https://specs.apollo.dev/connect/v0.2", import: [{name: "@source", as: "@api"}, {name: "JSONSelection", as: "Mapping"}, {name: "ConnectorErrors", as: "ErrorMappings"}, "ConnectHTTP"])

        directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

        directive @connect(source: String, http: ConnectHTTP, batch: connect__ConnectBatch, errors: ErrorMappings, isSuccess: Mapping, selection: Mapping!, entity: Boolean = false, id: String) repeatable on FIELD_DEFINITION | OBJECT

        directive @api(name: String!, http: connect__SourceHTTP, errors: ErrorMappings, isSuccess: Mapping) repeatable on SCHEMA

        type Query {
          hello: String
        }

        enum link__Purpose {
          SECURITY
          EXECUTION
        }

        scalar link__Import

        scalar Mapping

        scalar connect__URLTemplate

        input connect__HTTPHeaderMapping {
          name: String!
          from: String
          value: [String!]
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

        input connect__ConnectBatch {
          maxSize: Int
        }

        input ErrorMappings {
          message: Mapping
          extensions: Mapping
        }

        input connect__SourceHTTP {
          baseURL: String!
          headers: [connect__HTTPHeaderMapping!]
          path: Mapping
          queryParams: Mapping
        }
        "#);
    }

    #[test]
    fn test_v0_2_compatible_defs() {
        let schema = Schema::parse(r#"
        type Query { hello: String }
        extend schema
          @link(url: "https://specs.apollo.dev/link/v1.0")
          @link(url: "https://specs.apollo.dev/connect/v0.2")
        directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA
        enum link__Purpose { SECURITY EXECUTION }
        scalar link__Import

        scalar connect__URLTemplate
        scalar connect__JSONSelection
        input connect__ConnectHTTP {
          GET: connect__URLTemplate
        }
        directive @connect(source: String, http: connect__ConnectHTTP, selection: connect__JSONSelection!) repeatable on FIELD_DEFINITION
        "#, "schema.graphql").unwrap();

        let mut federation_schema = FederationSchema::new(schema).unwrap();
        let link = federation_schema
            .metadata()
            .unwrap()
            .for_identity(&ConnectSpec::identity())
            .unwrap();

        let spec = CONNECT_VERSIONS.find(&link.url.version).unwrap();
        spec.add_elements_to_schema(&mut federation_schema).unwrap();

        assert_snapshot!(federation_schema.schema().serialize().to_string(), @r#"
        schema {
          query: Query
        }

        extend schema @link(url: "https://specs.apollo.dev/link/v1.0") @link(url: "https://specs.apollo.dev/connect/v0.2")

        directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

        directive @connect(source: String, http: connect__ConnectHTTP, selection: connect__JSONSelection!) repeatable on FIELD_DEFINITION

        directive @connect__source(name: String!, http: connect__SourceHTTP, errors: connect__ConnectorErrors, isSuccess: connect__JSONSelection) repeatable on SCHEMA

        type Query {
          hello: String
        }

        enum link__Purpose {
          SECURITY
          EXECUTION
        }

        scalar link__Import

        scalar connect__URLTemplate

        scalar connect__JSONSelection

        input connect__ConnectHTTP {
          GET: connect__URLTemplate
        }

        input connect__HTTPHeaderMapping {
          name: String!
          from: String
          value: [String!]
        }

        input connect__ConnectBatch {
          maxSize: Int
        }

        input connect__ConnectorErrors {
          message: connect__JSONSelection
          extensions: connect__JSONSelection
        }

        input connect__SourceHTTP {
          baseURL: String!
          headers: [connect__HTTPHeaderMapping!]
          path: connect__JSONSelection
          queryParams: connect__JSONSelection
        }
        "#);
    }

    #[test]
    fn test_v0_2_incompatible_defs() {
        let schema = Schema::parse(r#"
        type Query { hello: String }
        extend schema
          @link(url: "https://specs.apollo.dev/link/v1.0")
          @link(url: "https://specs.apollo.dev/connect/v0.2")
        directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA
        enum link__Purpose { SECURITY EXECUTION }
        scalar link__Import

        scalar connect__URLTemplate
        scalar connect__JSONSelection
        input connect__ConnectHTTP {
          GET: connect__URLTemplate
        }
        directive @connect(source: String, http: connect__ConnectHTTP, selection: connect__JSONSelection!, entity: String!) repeatable on FIELD_DEFINITION
        "#, "schema.graphql").unwrap();

        let mut federation_schema = FederationSchema::new(schema).unwrap();
        let link = federation_schema
            .metadata()
            .unwrap()
            .for_identity(&ConnectSpec::identity())
            .unwrap();

        let spec = CONNECT_VERSIONS.find(&link.url.version).unwrap();
        let err = spec.add_elements_to_schema(&mut federation_schema).err();
        assert_snapshot!(err.unwrap().to_string(), @r###"Invalid definition for directive "@connect": argument "entity" should have type "Boolean" but found type "String""###)
    }
}
