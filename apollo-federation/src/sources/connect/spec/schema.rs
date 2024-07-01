use apollo_compiler::name;
use apollo_compiler::Name;
use indexmap::IndexMap;

use crate::schema::position::ObjectOrInterfaceFieldDirectivePosition;
use crate::sources::connect::json_selection::JSONSelection;

pub(crate) const CONNECT_DIRECTIVE_NAME_IN_SPEC: Name = name!("connect");
pub(crate) const CONNECT_SOURCE_ARGUMENT_NAME: Name = name!("source");
pub(crate) const CONNECT_HTTP_ARGUMENT_NAME: Name = name!("http");
pub(crate) const CONNECT_HTTP_ARGUMENT_GET_METHOD_NAME: Name = name!("GET");
pub(crate) const CONNECT_HTTP_ARGUMENT_POST_METHOD_NAME: Name = name!("POST");
pub(crate) const CONNECT_HTTP_ARGUMENT_PUT_METHOD_NAME: Name = name!("PUT");
pub(crate) const CONNECT_HTTP_ARGUMENT_PATCH_METHOD_NAME: Name = name!("PATCH");
pub(crate) const CONNECT_HTTP_ARGUMENT_DELETE_METHOD_NAME: Name = name!("DELETE");
pub(crate) const CONNECT_SELECTION_ARGUMENT_NAME: Name = name!("selection");
pub(crate) const CONNECT_ENTITY_ARGUMENT_NAME: Name = name!("entity");

pub(crate) const CONNECT_HTTP_NAME_IN_SPEC: Name = name!("ConnectHTTP");
pub(crate) const CONNECT_BODY_ARGUMENT_NAME: Name = name!("body");
pub(crate) const CONNECT_HEADERS_ARGUMENT_NAME: Name = name!("headers");

pub(crate) const SOURCE_DIRECTIVE_NAME_IN_SPEC: Name = name!("source");
pub(crate) const SOURCE_NAME_ARGUMENT_NAME: Name = name!("name");
pub(crate) const SOURCE_HTTP_ARGUMENT_NAME: Name = name!("http");

pub(crate) const SOURCE_HTTP_NAME_IN_SPEC: Name = name!("SourceHTTP");
pub(crate) const SOURCE_BASE_URL_ARGUMENT_NAME: Name = name!("baseURL");
pub(crate) const SOURCE_HEADERS_ARGUMENT_NAME: Name = name!("headers");

pub(crate) const HTTP_HEADER_MAPPING_NAME_IN_SPEC: Name = name!("HTTPHeaderMapping");
pub(crate) const HTTP_HEADER_MAPPING_NAME_ARGUMENT_NAME: Name = name!("name");
pub(crate) const HTTP_HEADER_MAPPING_AS_ARGUMENT_NAME: Name = name!("as");
pub(crate) const HTTP_HEADER_MAPPING_VALUE_ARGUMENT_NAME: Name = name!("value");

pub(crate) const JSON_SELECTION_SCALAR_NAME: Name = name!("JSONSelection");
pub(crate) const URL_PATH_TEMPLATE_SCALAR_NAME: Name = name!("URLPathTemplate");

/// Arguments to the `@source` directive
///
/// Refer to [SourceSpecDefinition] for more info.
#[cfg_attr(test, derive(Debug))]
pub(crate) struct SourceDirectiveArguments {
    /// The friendly name of this source for use in `@connect` directives
    pub(crate) name: String,

    /// Common HTTP options
    pub(crate) http: SourceHTTPArguments,
}

/// Common HTTP options for a connector [SourceSpecDefinition]
#[cfg_attr(test, derive(Debug))]
pub(crate) struct SourceHTTPArguments {
    /// The base URL containing all sub API endpoints
    pub(crate) base_url: String,

    /// HTTP headers used when requesting resources from the upstream source.
    /// Can be overridden by name with headers in a @connect directive.
    pub(crate) headers: HTTPHeaderMappings,
}

/// Map of HTTP header names to configuration options
#[cfg_attr(test, derive(Debug))]
#[derive(derive_more::Deref, Default)]
pub(crate) struct HTTPHeaderMappings(pub(crate) IndexMap<String, Option<HTTPHeaderOption>>);

/// Configuration option for an HTTP header
#[derive(Debug, Clone)]
pub enum HTTPHeaderOption {
    /// The alias for the header name used when making a request
    ///
    /// This assumes that a header exists in the original request, as it will be
    /// renamed to the supplied value.
    As(String),

    /// The raw values to use for the HTTP header
    Value(Vec<String>),
}

/// Arguments to the `@connect` directive
///
/// Refer to [ConnectSpecDefinition] for more info.
#[cfg_attr(test, derive(Debug))]
pub(crate) struct ConnectDirectiveArguments {
    pub(crate) position: ObjectOrInterfaceFieldDirectivePosition,

    /// The upstream source for shared connector configuration.
    ///
    /// Must match the `name` argument of a @source directive in this schema.
    pub(crate) source: Option<String>,

    /// HTTP options for this connector
    ///
    /// Marked as optional in the GraphQL schema to allow for future transports,
    /// but is currently required.
    pub(crate) http: Option<ConnectHTTPArguments>,

    /// Fields to extract from the upstream JSON response.
    ///
    /// Uses the JSONSelection syntax to define a mapping of connector response to
    /// GraphQL schema.
    pub(crate) selection: JSONSelection,

    /// Entity resolver marker
    ///
    /// Marks this connector as a canonical resolver for an entity (uniquely
    /// identified domain model.) If true, the connector must be defined on a field
    /// of the Query type.
    pub(crate) entity: bool,
}

/// The HTTP arguments needed for a connect request
#[cfg_attr(test, derive(Debug))]
pub(crate) struct ConnectHTTPArguments {
    pub(crate) get: Option<String>,
    pub(crate) post: Option<String>,
    pub(crate) patch: Option<String>,
    pub(crate) put: Option<String>,
    pub(crate) delete: Option<String>,

    /// Request body
    ///
    /// Define a request body using JSONSelection. Selections can include values from
    /// field arguments using `$args.argName` and from fields on the parent type using
    /// `$this.fieldName`.
    pub(crate) body: Option<JSONSelection>,

    /// Configuration for headers to attach to the request.
    ///
    /// Overrides headers from the associated @source by name.
    pub(crate) headers: HTTPHeaderMappings,
}
