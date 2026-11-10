use std::borrow::Cow;
use std::sync::Arc;

use opentelemetry::Key;
use schemars::JsonSchema;
use schemars::Schema;
use schemars::SchemaGenerator;
use serde::Deserialize;
use serde::Deserializer;

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
        alias: AttributeAlias,
    },
}

impl StandardAttribute {
    pub(crate) fn key(&self, original_key: Key) -> Option<Key> {
        match self {
            StandardAttribute::Bool(true) => Some(original_key),
            StandardAttribute::Aliased { alias } => Some(alias.0.clone()),
            _ => None,
        }
    }
}

/// A standard attribute's alias, the key its attribute is recorded under in place of its
/// standard name: made once from the configuration's string and shared by every attribute
/// recorded under it, so making one copies no string. The configuration states it as a string.
#[derive(Clone, PartialEq)]
pub(crate) struct AttributeAlias(Key);

impl From<&str> for AttributeAlias {
    fn from(alias: &str) -> Self {
        Self(Key::from(Arc::<str>::from(alias)))
    }
}

impl std::fmt::Debug for AttributeAlias {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self.0.as_str(), f)
    }
}

impl<'de> Deserialize<'de> for AttributeAlias {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer).map(|alias| Self::from(alias.as_str()))
    }
}

impl JsonSchema for AttributeAlias {
    fn inline_schema() -> bool {
        String::inline_schema()
    }

    fn schema_name() -> Cow<'static, str> {
        String::schema_name()
    }

    fn schema_id() -> Cow<'static, str> {
        String::schema_id()
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        String::json_schema(generator)
    }
}

impl apollo_configuration::Validate for AttributeAlias {}
