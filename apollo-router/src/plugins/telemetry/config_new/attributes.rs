use opentelemetry::Key;

pub(crate) const HTTP_REQUEST_RESEND_COUNT: Key = Key::from_static_str("http.request.resend_count");

pub(crate) const ERROR_TYPE: Key = Key::from_static_str("error.type");

pub(crate) const NETWORK_LOCAL_ADDRESS: Key = Key::from_static_str("network.local.address");
pub(crate) const NETWORK_LOCAL_PORT: Key = Key::from_static_str("network.local.port");

pub(crate) const NETWORK_PEER_ADDRESS: Key = Key::from_static_str("network.peer.address");
pub(crate) const NETWORK_PEER_PORT: Key = Key::from_static_str("network.peer.port");

pub(crate) const HTTP_REQUEST_HEADERS: Key = Key::from_static_str("http.request.headers");
pub(crate) const HTTP_REQUEST_URI: Key = Key::from_static_str("http.request.uri");
pub(crate) const HTTP_REQUEST_VERSION: Key = Key::from_static_str("http.request.version");
pub(crate) const HTTP_REQUEST_BODY: Key = Key::from_static_str("http.request.body");

pub(crate) const HTTP_RESPONSE_HEADERS: Key = Key::from_static_str("http.response.headers");
pub(crate) const HTTP_RESPONSE_STATUS: Key = Key::from_static_str("http.response.status");
pub(crate) const HTTP_RESPONSE_VERSION: Key = Key::from_static_str("http.response.version");
pub(crate) const HTTP_RESPONSE_BODY: Key = Key::from_static_str("http.response.body");

#[apollo_configuration::configuration]
#[derive(Copy)]
pub(crate) enum DefaultAttributeRequirementLevel {
    /// No default attributes set on spans, you have to set it one by one in the configuration to enable some attributes
    None,
    /// Attributes that are marked as required in otel semantic conventions and apollo documentation will be included (default)
    #[config(default)]
    Required,
    /// Attributes that are marked as required or recommended in otel semantic conventions and apollo documentation will be included
    Recommended,
}

#[apollo_configuration::configuration]
#[derive(PartialEq)]
#[serde(untagged)]
pub(crate) enum StandardAttribute {
    Bool(bool),
    Aliased {
        #[config(required)]
        alias: String,
    },
}

impl StandardAttribute {
    pub(crate) fn key(&self, original_key: Key) -> Option<Key> {
        match self {
            StandardAttribute::Bool(true) => Some(original_key),
            StandardAttribute::Aliased { alias } => Some(Key::new(alias.clone())),
            _ => None,
        }
    }
}
