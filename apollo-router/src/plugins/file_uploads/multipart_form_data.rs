use bytes::Bytes;
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

use super::MapField;
use super::MultipartRequest;
use super::UploadResult;

pub(super) struct MultipartFormData {
    boundary: String,
    operations: hyper::Body,
    map: MapField,
    multipart: MultipartRequest,
}

impl MultipartFormData {
    pub(super) fn new(operations: hyper::Body, map: MapField, multipart: MultipartRequest) -> Self {
        let boundary = format!(
            "------------------------{:016x}",
            rand::thread_rng().next_u64()
        );
        Self {
            boundary,
            operations,
            map,
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

    pub(super) async fn into_stream(self) -> impl Stream<Item = UploadResult<Bytes>> {
        fn field(
            boundary: &str,
            name: &str,
            value_stream: impl Stream<Item = UploadResult<Bytes>>,
        ) -> impl Stream<Item = UploadResult<Bytes>> {
            let prefix = format!(
                "--{}\r\nContent-Disposition: form-data; name=\"{}\"\r\n\r\n",
                boundary, name
            )
            .into();

            tokio_stream::once(Ok(prefix))
                .chain(value_stream)
                .chain(tokio_stream::once(Ok("\r\n".into())))
        }

        let Self {
            boundary,
            operations,
            map,
            mut multipart,
        } = self;
        let last = tokio_stream::once(Ok(format!("--{}--\r\n", boundary).into()));

        let operations_field = field(&boundary, "operations", operations.map_err(Into::into));
        let map_bytes = serde_json::to_vec(&map)
            .expect("map should be serializable to JSON")
            .into();
        let map_field = field(&boundary, "map", tokio_stream::once(Ok(map_bytes)));

        let files = map.into_keys().collect();
        let before_file = move |headers: &HeaderMap| {
            let mut prefix = Vec::new();
            prefix.extend_from_slice(b"--");
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
        let after_file = || Bytes::from_static(b"\r\n");
        let file_fields = multipart
            .subgraph_stream(before_file, files, after_file)
            .await;

        operations_field.chain(map_field).chain(file_fields).chain(last)
    }
}
