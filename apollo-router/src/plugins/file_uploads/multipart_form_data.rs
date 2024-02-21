use std::sync::Arc;

use bytes::Bytes;
use bytes::BytesMut;
use futures::stream::StreamExt;
use futures::stream::TryStreamExt;
use futures::Stream;
use http::HeaderMap;
use http::HeaderValue;
use mediatype::names::BOUNDARY;
use mediatype::names::FORM_DATA;
use mediatype::names::MULTIPART;
use mediatype::MediaType;
use rand::RngCore;

use super::map_field::MapFieldRaw;
use super::MultipartRequest;
use super::Result as UploadResult;

#[derive(Clone, Debug)]
pub(super) struct MultipartFormData {
    boundary: String,
    map: Arc<MapFieldRaw>,
    multipart: MultipartRequest,
}

impl MultipartFormData {
    pub(super) fn new(map: MapFieldRaw, multipart: MultipartRequest) -> Self {
        let boundary = format!("{:016x}", rand::thread_rng().next_u64());
        Self {
            boundary,
            map: Arc::new(map),
            multipart,
        }
    }

    pub(super) fn content_type(&self) -> HeaderValue {
        let boundary =
            mediatype::Value::new(&self.boundary).expect("boundary should be valid value");
        let params = [(BOUNDARY, boundary)];
        let mime = MediaType::from_parts(MULTIPART, FORM_DATA, None, &params);
        mime.to_string()
            .try_into()
            .expect("mime should be valid header value")
    }

    pub(super) async fn into_stream(
        mut self,
        operations: hyper::Body,
    ) -> impl Stream<Item = UploadResult<Bytes>> {
        let map_bytes =
            serde_json::to_string(&self.map).expect("map should be serializable to JSON");
        let field_prefix = |name: &str| {
            format!(
                "--{}\r\nContent-Disposition: form-data; name=\"{}\"\r\n\r\n",
                self.boundary, name
            )
        };

        let static_part = tokio_stream::once(Ok(Bytes::from(field_prefix("operations"))))
            .chain(operations.map_err(Into::into))
            .chain(tokio_stream::once(Ok(Bytes::from(format!(
                "\r\n{}{}\r\n",
                field_prefix("map"),
                map_bytes
            )))));

        let last = tokio_stream::once(Ok(format!("\r\n--{}--\r\n", self.boundary).into()));

        let file_names = self.map.keys().cloned().collect();
        let boundary = self.boundary;
        let file_prefix = move |headers: &HeaderMap| {
            let mut prefix = BytesMut::new();
            prefix.extend_from_slice(b"\r\n--");
            prefix.extend_from_slice(boundary.as_bytes());
            prefix.extend_from_slice(b"\r\n");
            for (k, v) in headers.iter() {
                prefix.extend_from_slice(k.as_str().as_bytes());
                prefix.extend_from_slice(b": ");
                prefix.extend_from_slice(v.as_bytes());
                prefix.extend_from_slice(b"\r\n");
            }
            prefix.extend_from_slice(b"\r\n");

            Bytes::from(prefix)
        };

        let files_stream = self
            .multipart
            .subgraph_stream(file_names, file_prefix)
            .await;
        static_part.chain(files_stream).chain(last)
    }
}
