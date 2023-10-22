use crate::plugins::telemetry::config_new::attributes::{
    Extendable, RouterAttributes, RouterCustomAttribute, RouterEvent, SubgraphAttributes,
    SubgraphCustomAttribute, SupergraphAttributes, SupergraphCustomAttribute,
};
use schemars::JsonSchema;
use serde::Deserialize;

#[allow(dead_code)]
#[derive(Clone, Deserialize, JsonSchema, Debug, Default)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct Instruments {
    /// Router service instruments. For more information see documentation on Router lifecycle.
    router: Extendable<RouterInstruments, Instrument<RouterAttributes, RouterCustomAttribute>>,
    /// Supergraph service instruments. For more information see documentation on Router lifecycle.
    supergraph: Extendable<
        SupergraphInstruments,
        Instrument<SupergraphAttributes, SupergraphCustomAttribute>,
    >,
    /// Subgraph service instruments. For more information see documentation on Router lifecycle.
    subgraph:
        Extendable<SubgraphInstruments, Instrument<SubgraphAttributes, SubgraphCustomAttribute>>,
}

#[allow(dead_code)]
#[derive(Clone, Deserialize, JsonSchema, Debug, Default)]
#[serde(deny_unknown_fields, default)]
struct RouterInstruments {
    /// Histogram of server request duration
    #[serde(rename = "http.server.request.duration")]
    http_server_request_duration: bool,

    /// Gauge of active requests
    #[serde(rename = "http.server.active_requests")]
    http_server_active_requests: bool,

    /// Histogram of server request body size
    #[serde(rename = "http.server.request.body.size")]
    http_server_request_body_size: bool,

    /// Histogram of server response body size
    #[serde(rename = "http.server.response.body.size")]
    http_server_response_body_size: bool,
}

#[allow(dead_code)]
#[derive(Clone, Deserialize, JsonSchema, Debug, Default)]
#[serde(deny_unknown_fields, default)]
struct SupergraphInstruments {}

#[allow(dead_code)]
#[derive(Clone, Deserialize, JsonSchema, Debug, Default)]
#[serde(deny_unknown_fields, default)]
struct SubgraphInstruments {
    /// Histogram of client request duration
    #[serde(rename = "http.client.request.duration")]
    http_client_request_duration: bool,

    /// Histogram of client request body size
    #[serde(rename = "http.client.request.body.size")]
    http_client_request_body_size: bool,

    /// Histogram of client response body size
    #[serde(rename = "http.client.response.body.size")]
    http_client_response_body_size: bool,
}

#[allow(dead_code)]
#[derive(Clone, Deserialize, JsonSchema, Debug)]
pub(crate) struct Instrument<A, E>
where
    A: Default,
{
    /// The type of instrument.
    #[serde(rename = "type")]
    ty: InstrumentType,

    /// The router event to instrument.
    event: RouterEvent,

    /// The description of the instrument.
    description: String,

    /// The units of the instrument, e.g. "ms", "bytes", "requests".
    unit: String,

    /// Attributes to include on the instrument.
    #[serde(default = "Extendable::empty::<A, E>")]
    attributes: Extendable<A, E>,
}

#[allow(dead_code)]
#[derive(Clone, Deserialize, JsonSchema, Debug)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) enum InstrumentType {
    /// A monotonic counter
    Counter,

    /// A duration histogram
    Duration,
}
