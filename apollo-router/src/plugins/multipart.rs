use std::ops::ControlFlow;

use http::StatusCode;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;

use crate::error::Error;
use crate::layers::ServiceBuilderExt;
use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::register_plugin;
use crate::services::supergraph;

#[derive(Debug, Clone)]
struct Multipart {}

#[async_trait::async_trait]
impl Plugin for Multipart {
    type Config = ();

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        Ok(Self {})
    }

    fn supergraph_service(&self, service: supergraph::BoxService) -> supergraph::BoxService {
        ServiceBuilder::new()
           
            .map_response(|res: supergraph::Response| {
                            if !res.has_next.unwrap_or(false)
                                && (accepts_json || accepts_wildcard)
                            {
                                parts.headers.insert(
                                    CONTENT_TYPE,
                                    HeaderValue::from_static("application/json"),
                                );
                                tracing::trace_span!("serialize_response").in_scope(|| {
                                    http_ext::Response::from(http::Response::from_parts(
                                        parts, response,
                                    ))
                                    .into_response()
                                })
                            } else if accepts_multipart {
                                parts.headers.insert(
                                    CONTENT_TYPE,
                                    HeaderValue::from_static(MULTIPART_DEFER_CONTENT_TYPE),
                                );

                                // each chunk contains a response and the next delimiter, to let client parsers
                                // know that they can process the response right away
                                let mut first_buf = Vec::from(
                                    &b"\r\n--graphql\r\ncontent-type: application/json\r\n\r\n"[..],
                                );
                                serde_json::to_writer(&mut first_buf, &response).unwrap();
                                if response.has_next.unwrap_or(false) {
                                    first_buf.extend_from_slice(b"\r\n--graphql\r\n");
                                } else {
                                    first_buf.extend_from_slice(b"\r\n--graphql--\r\n");
                                }

                                let body = once(ready(Ok(Bytes::from(first_buf)))).chain(
                                    stream.map(|res| {
                                        let mut buf = Vec::from(
                                            &b"content-type: application/json\r\n\r\n"[..],
                                        );
                                        serde_json::to_writer(&mut buf, &res).unwrap();

                                        // the last chunk has a different end delimiter
                                        if res.has_next.unwrap_or(false) {
                                            buf.extend_from_slice(b"\r\n--graphql\r\n");
                                        } else {
                                            buf.extend_from_slice(b"\r\n--graphql--\r\n");
                                        }

                                        Ok::<_, BoxError>(buf.into())
                                    }),
                                );

                                (parts, StreamBody::new(body)).into_response()
            })
            .service(service)
            .boxed()
    }
}

register_plugin!("apollo", "multipart", Multipart);
