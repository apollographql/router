use apollo_compiler::schema::Name;
use apollo_compiler::{name, NodeStr};

use crate::link::spec::{Url, Version};

pub(crate) const SOURCE_DIRECTIVE_NAME_IN_SPEC: Name = name!("source");
pub(crate) const SOURCE_NAME_ARGUMENT_NAME: Name = name!("name");
pub(crate) const SOURCE_HTTP_ARGUMENT_NAME: Name = name!("http");
pub(crate) const SOURCE_BASE_URL_ARGUMENT_NAME: Name = name!("baseURL");

pub(crate) const CONNECT_DIRECTIVE_NAME_IN_SPEC: Name = name!("connect");
pub(crate) const CONNECT_SOURCE_ARGUMENT_NAME: Name = name!("source");
pub(crate) const CONNECT_HTTP_ARGUMENT_NAME: Name = name!("http");
pub(crate) const CONNECT_SELECTION_ARGUMENT_NAME: Name = name!("selection");

/// A directive to model an upstream HTTP data source
///
/// A source describes an HTTP endpoint that contains data that can be scraped
/// and automatically transformed into GraphQL objects.
///
/// The relevant GraphQL directive takes [SourceSpecDefinition] as arguments and
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
    /// The owning subgraph
    pub(crate) graph: NodeStr,

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
