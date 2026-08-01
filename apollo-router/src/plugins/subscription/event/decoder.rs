use serde_json_bytes::ByteString;
use serde_json_bytes::Map;
use serde_json_bytes::Value;

use crate::error::Error;
use crate::graphql;

use super::ProviderEvent;

pub(super) fn decode_graphql_entity(
    event: ProviderEvent,
    response_name: &str,
) -> graphql::Response {
    match serde_json::from_slice::<Value>(&event.payload) {
        Ok(value @ Value::Object(_)) if value.get("__typename").is_some() => {
            let mut data = Map::new();
            data.insert(ByteString::from(response_name), value);
            graphql::Response::builder()
                .data(Value::Object(data))
                .build()
        }
        Ok(Value::Object(_)) => graphql::Response::builder()
            .error(
                Error::builder()
                    .message("event payload is missing required '__typename'")
                    .extension_code("EVENT_DECODE_ERROR")
                    .build(),
            )
            .build(),
        Ok(_) => graphql::Response::builder()
            .error(
                Error::builder()
                    .message("event payload must be a JSON object")
                    .extension_code("EVENT_DECODE_ERROR")
                    .build(),
            )
            .build(),
        Err(error) => graphql::Response::builder()
            .error(
                Error::builder()
                    .message(format!("event payload is not valid JSON: {error}"))
                    .extension_code("EVENT_DECODE_ERROR")
                    .build(),
            )
            .build(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_entity_under_response_field() {
        let response = decode_graphql_entity(
            ProviderEvent {
                payload: bytes::Bytes::from_static(br#"{"__typename":"Product","id":"1"}"#),
            },
            "productUpdated",
        );
        assert_eq!(
            response.data,
            Some(serde_json_bytes::json!({
                "productUpdated": {"__typename": "Product", "id": "1"}
            }))
        );
    }

    #[test]
    fn rejects_entity_without_typename() {
        let response = decode_graphql_entity(
            ProviderEvent {
                payload: bytes::Bytes::from_static(br#"{"id":"1"}"#),
            },
            "productUpdated",
        );
        assert_eq!(
            response.errors[0].extension_code(),
            Some("EVENT_DECODE_ERROR".to_string())
        );
    }
}
