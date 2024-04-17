use apollo_compiler::schema::Name;
use apollo_compiler::{name, NodeStr};

use crate::link::spec::{Url, Version};

pub(crate) const SOURCE_DIRECTIVE_NAME_IN_SPEC: Name = name!("source");
pub(crate) const SOURCE_NAME_ARGUMENT_NAME: Name = name!("name");
pub(crate) const SOURCE_HTTP_ARGUMENT_NAME: Name = name!("http");
pub(crate) const SOURCE_BASE_URL_ARGUMENT_NAME: Name = name!("baseURL");
pub(crate) const SOURCE_HEADERS_ARGUMENT_NAME: Name = name!("headers");

pub(crate) const CONNECT_DIRECTIVE_NAME_IN_SPEC: Name = name!("connect");
pub(crate) const CONNECT_SOURCE_ARGUMENT_NAME: Name = name!("source");
pub(crate) const CONNECT_HTTP_ARGUMENT_NAME: Name = name!("http");
pub(crate) const CONNECT_SELECTION_ARGUMENT_NAME: Name = name!("selection");
pub(crate) const CONNECT_ENTITY_ARGUMENT_NAME: Name = name!("entity");
pub(crate) const CONNECT_BODY_ARGUMENT_NAME: Name = name!("body");
pub(crate) const CONNECT_HEADERS_ARGUMENT_NAME: Name = name!("headers");

/// A directive to model an upstream HTTP data source
///
/// A source describes an HTTP endpoint that contains data that can be scraped
/// and automatically transformed into GraphQL objects.
///
/// The relevant GraphQL directive takes [SourceDirectiveArguments] as arguments and
/// is mainly used to allow for sharing common HTTP parameters, allowing for a
/// more DRY subgraph configuration.
#[cfg_attr(test, derive(Debug))]
pub(crate) struct SourceSpecDefinition {
    /// The URL as shown in the @link import
    url: Url,

    /// The minimum version of federation needed for @source
    minimum_federation_version: Option<Version>,
}

/// Arguments to the `@source` directive
///
/// Refer to [SourceSpecDefinition] for more info.
#[cfg_attr(test, derive(Debug))]
pub(crate) struct SourceDirectiveArguments {
    /// The friendly name of this source for use in `@connect` directives
    pub(crate) name: NodeStr,

    /// Common HTTP options
    pub(crate) http: HTTPArguments,
}

/// Common HTTP options for a connector [SourceSpecDefinition]
#[cfg_attr(test, derive(Debug))]
pub(crate) struct HTTPArguments {
    /// The base URL containing all sub API endpoints
    pub(crate) base_url: NodeStr,

    /// HTTP headers used when requesting resources from the upstream source
    // TODO: The spec says that this is nullable, so should we encode this as
    // an `Option` or do we not care if null maps to an empty list?
    pub(crate) headers: Vec<HTTPHeaderMapping>,
}

/// Pairs of HTTP header names to configuration options
#[cfg_attr(test, derive(Debug))]
pub(crate) struct HTTPHeaderMapping {
    /// The HTTP header name to propagate
    pub(crate) name: NodeStr,

    /// Configuration option for the HTTP header name supplied above
    pub(crate) option: Option<HTTPHeaderOption>,
}

/// Configuration option for an HTTP header
#[cfg_attr(test, derive(Debug))]
pub(crate) enum HTTPHeaderOption {
    /// The alias for the header name used when making a request
    ///
    /// This assumes that a header exists in the original request, as it will be
    /// renamed to the supplied value.
    As(NodeStr),

    /// The raw value to use for the HTTP header
    Value(NodeStr),
}

/// A directive to model an upstream HTTP data type
///
/// A connect describes an HTTP endpoint that contains a specific data type and
/// semantic transformers needed to convert them to their GraphQL representation.
///
/// The relevant GraphQL directive takes [ConnectDirectiveArguments] as arguments and
/// defines the upstream source (which can optionally be specified as a [SourceSpecDefinition]),
/// transformers for consuming the necessary data, and HTTP semantics for the actual request.
#[cfg_attr(test, derive(Debug))]
pub(crate) struct ConnectSpecDefinition {
    /// The URL as shown in the @link import
    url: Url,

    /// The minimum version of federation needed for @source
    minimum_federation_version: Option<Version>,
}

/// Arguments to the `@connect` directive
///
/// Refer to [ConnectSpecDefinition] for more info.
#[cfg_attr(test, derive(Debug))]
pub(crate) struct ConnectDirectiveArguments {
    /// The upstream source for shared connector configuration.
    ///
    /// Must match the `name` argument of a @source directive in this schema.
    pub(crate) source: Option<NodeStr>,

    /// HTTP configuration for the actual request
    pub(crate) http: ConnectHTTPArguments,

    /// Fields to extract from the upstream JSON response.
    ///
    /// Uses the JSONSelection syntax to define a mapping of connector response to
    /// GraphQL schema.
    pub(crate) selection: Option<JSONSelection>,

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
    /// The HTTP verb for the connect request
    pub(crate) method: HTTPMethod,

    /// The URL to query, along with template parameters
    ///
    /// Can be a full URL or a partial path. If it's a partial path, it will be
    /// appended to an associated `baseURL` from the related @source.
    pub(crate) url: URLPathTemplate,

    /// Request body
    ///
    /// Define a request body using JSONSelection. Selections can include values from
    /// field arguments using `$args.argName` and from fields on the parent type using
    /// `$this.fieldName`.
    pub(crate) body: Option<JSONSelection>,

    /// Configuration for headers to attach to the request.
    ///
    /// Takes precedence over headers defined on the associated @source.
    // TODO: The spec says that this is nullable, so should we encode this as
    // an `Option` or do we not care if null maps to an empty list?
    pub(crate) headers: Vec<HTTPHeaderMapping>,
}

/// The HTTP arguments needed for a connect request
#[cfg_attr(test, derive(Debug))]
pub(crate) enum HTTPMethod {
    Get,
    Post,
    Patch,
    Put,
    Delete,
}

/// A JSON selection set
///
/// A string containing a "JSON Selection", which defines a mapping from one JSON-like
/// shape to another JSON-like shape.
///
/// Example: ".data { id: user_id name account: { id: account_id } }"
// TODO: This should probably be broken out into an actual struct with useful fields.
#[cfg_attr(test, derive(Debug))]
pub(crate) struct JSONSelection(pub(crate) String);

/// A URL with template parameters
///
/// A string that declares a URL path with values interpolated inside `{}`.
///
/// Example: "/product/{$this.id}/reviews?count={$args.count}"
// TODO: This should probably be broken out into an actual struct with useful fields.
#[cfg_attr(test, derive(Debug))]
pub(crate) struct URLPathTemplate(pub(crate) String);
