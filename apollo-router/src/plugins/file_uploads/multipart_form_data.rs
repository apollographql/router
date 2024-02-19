use bytes::Bytes;
use futures::stream::StreamExt;
use futures::stream::TryStreamExt;
use futures::Stream;
use http::HeaderValue;
use mediatype::names::BOUNDARY;
use mediatype::names::FORM_DATA;
use mediatype::names::MULTIPART;
use mediatype::MediaType;
use rand::RngCore;

use super::MultipartField;
use super::SubgraphFileProxyStream;
use super::UploadResult;

pub(super) struct MultipartFormData {
    boundary: String,
    operations: hyper::Body,
    map: Bytes,
    files: SubgraphFileProxyStream,
}

impl MultipartFormData {
    pub(super) fn new(operations: hyper::Body, map: Bytes, files: SubgraphFileProxyStream) -> Self {
        let boundary = format!(
            "------------------------{:016x}",
            rand::thread_rng().next_u64()
        );
        Self {
            boundary,
            operations,
            map,
            files,
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

    pub(super) fn into_stream(self) -> impl Stream<Item = UploadResult<Bytes>> {
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

        fn file<'a>(
            boundary: &str,
            field: MultipartField,
        ) -> impl Stream<Item = UploadResult<Bytes>> + 'a {
            let mut prefix = Vec::new();
            prefix.extend_from_slice(b"--");
            prefix.extend_from_slice(boundary.as_bytes());
            prefix.extend_from_slice(b"\r\n");
            for (k, v) in field.headers().iter() {
                prefix.extend_from_slice(k.as_str().as_bytes());
                prefix.extend_from_slice(b": ");
                prefix.extend_from_slice(v.as_bytes());
                prefix.extend_from_slice(b"\r\n");
            }
            prefix.extend_from_slice(b"\r\n");

            tokio_stream::once(Ok(Bytes::from(prefix)))
                .chain(field)
                .chain(tokio_stream::once(Ok("\r\n".into())))
        }

        let Self {
            boundary,
            operations,
            map,
            files,
        } = self;
        let last = tokio_stream::once(Ok(format!("--{}--\r\n", boundary).into()));

        field(&boundary, "operations", operations.map_err(Into::into))
            .chain(field(&boundary, "map", tokio_stream::once(Ok(map))))
            .chain(files.flat_map(move |field| match field {
                Ok(field) => file(&boundary, field).left_stream(),
                Err(e) => tokio_stream::once(Err(e)).right_stream(),
            }))
            .chain(last)
    }
}
