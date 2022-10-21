//! Our formatters and visitors used for logging
pub(crate) mod json;
pub(crate) mod text;

pub(crate) use json::JsonFields;
pub(crate) use text::TextFormatter;

pub(crate) const REQUEST_ID_FIELD_NAME: &str = "request_id";
