use super::*;

#[test]
fn test_error_types_debug_and_display() {
    let invalid_request_error = Error::InvalidRequest {
        details: "Test details".to_string(),
    };
    
    let response_processing_error = Error::ResponseProcessingFailed {
        source: Box::new(std::io::Error::new(
            std::io::ErrorKind::Other,
            "test error"
        )),
    };
    
    // Test that errors can be formatted
    assert!(!format!("{:?}", invalid_request_error).is_empty());
    assert!(!format!("{}", invalid_request_error).is_empty());
    
    assert!(!format!("{:?}", response_processing_error).is_empty());
    assert!(!format!("{}", response_processing_error).is_empty());
}

#[test]
fn test_request_response_type_aliases() {
    // Test that the type aliases can be used to construct request/response types
    use bytes::Bytes;
    use http_body_util::{combinators::UnsyncBoxBody, BodyExt, Full};
    
    let body = UnsyncBoxBody::new(Full::new(Bytes::from("test")).map_err(|_: std::convert::Infallible| unreachable!()));
    let _request: Request = http::Request::builder()
        .method("GET")
        .uri("http://example.com")
        .body(body)
        .unwrap();
    
    let body = UnsyncBoxBody::new(Full::new(Bytes::from("response")).map_err(|_: std::convert::Infallible| unreachable!()));
    let _response: Response = http::Response::builder()
        .status(200)
        .body(body)
        .unwrap();
} 