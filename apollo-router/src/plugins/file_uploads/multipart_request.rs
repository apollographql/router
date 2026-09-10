use core::task;
use std::collections::HashSet;
use std::mem;
use std::pin::Pin;
use std::sync::Arc;
use std::task::Poll;

use bytes::Bytes;
use futures::Stream;
use http::HeaderMap;
use http_body_util::BodyExt;
use itertools::Itertools;
use multer::Constraints;
use multer::Multipart;
use multer::SizeLimit;
use pin_project_lite::pin_project;
use tokio::sync::Mutex;
use tokio::sync::OwnedMutexGuard;

use super::Result as UploadResult;
use super::config::MultipartRequestLimits;
use super::error::FileUploadError;
use super::map_field::MapField;
use super::map_field::MapFieldRaw;
use crate::services::router::body::RouterBody;

// The limit to set for the map field in the multipart request.
// We don't expect this to ever be reached, but we can always add a config option if needed later.
const MAP_SIZE_LIMIT: u64 = 10 * 1024;

#[derive(Clone, Debug)]
pub(super) struct MultipartRequest {
    state: Arc<Mutex<MultipartRequestState>>,
}

#[derive(Debug)]
struct MultipartRequestState {
    multer: multer::Multipart<'static>,
    limits: MultipartRequestLimits,
    read_files_counter: usize,
    file_sizes: Vec<usize>,
    max_files_exceeded: bool,
    max_files_size_exceeded: bool,
}

impl Drop for MultipartRequestState {
    fn drop(&mut self) {
        u64_counter!(
            "apollo.router.operations.file_uploads",
            "file uploads",
            1,
            "file_uploads.limits.max_file_size.exceeded" = self.max_files_size_exceeded,
            "file_uploads.limits.max_files.exceeded" = self.max_files_exceeded
        );

        for file_size in &self.file_sizes {
            u64_histogram!(
                "apollo.router.operations.file_uploads.file_size",
                "file upload sizes",
                (*file_size) as u64
            );
        }
        u64_histogram!(
            "apollo.router.operations.file_uploads.files",
            "number of files per request",
            self.read_files_counter as u64
        );
    }
}

/// The budget for multer's `whole_stream` limit, which is the only limit that bounds the bytes
/// the parser buffers outside of field content: the preamble, per-part header blocks, boundary
/// delimiters and transport padding. multer's per-field limits count content bytes only, so
/// without this those regions are unbounded and a single request can exhaust router memory.
///
/// The budget is the content the other limits already permit, plus `max_overhead_size`. multer
/// cannot tell a framing byte from a content byte, so the two cannot be limited separately. See
/// `max_overhead_size` in config.rs for what this does and does not guarantee.
///
/// The arithmetic saturates. `max_files * max_file_size` overflows `u64` for limits operators
/// really write, because `max_file_size` is often set in gigabytes.
///
/// A saturated budget is `u64::MAX`, which is not a sentinel for "no limit". multer tests
/// `bytes_pulled > limit`, so the test never passes and the limit stops working. Only an absurd
/// configuration saturates. The unit tests below pin both halves: `large.router.yaml`-scale limits
/// must not saturate, and extreme ones must degrade to no limit rather than wrap to a tiny budget.
fn whole_stream_size_limit(limits: &MultipartRequestLimits, operations_size_limit: u64) -> u64 {
    operations_size_limit
        .saturating_add(MAP_SIZE_LIMIT)
        .saturating_add((limits.max_files as u64).saturating_mul(limits.max_file_size.as_u64()))
        .saturating_add(limits.max_overhead_size.as_u64())
}

impl MultipartRequest {
    pub(super) fn new(
        request_body: RouterBody,
        boundary: String,
        limits: MultipartRequestLimits,
        operations_size_limit: u64,
    ) -> Self {
        let multer = Multipart::with_constraints(
            request_body.into_data_stream(),
            boundary,
            Constraints::new().size_limit(
                SizeLimit::new()
                    .whole_stream(whole_stream_size_limit(&limits, operations_size_limit))
                    .for_field("map", MAP_SIZE_LIMIT)
                    .for_field("operations", operations_size_limit),
            ),
        );
        Self {
            state: Arc::new(Mutex::new(MultipartRequestState {
                multer,
                limits,
                read_files_counter: 0,
                file_sizes: Vec::new(),
                max_files_exceeded: false,
                max_files_size_exceeded: false,
            })),
        }
    }

    pub(super) async fn operations_field(&mut self) -> UploadResult<multer::Field<'static>> {
        self.state
            .lock()
            .await
            .multer
            .next_field()
            .await?
            .filter(|field| field.name() == Some("operations"))
            .ok_or_else(|| FileUploadError::MissingOperationsField)
    }

    pub(super) async fn map_field(&mut self) -> UploadResult<MapField> {
        let mut state = self.state.lock().await;
        let bytes = state
            .multer
            .next_field()
            .await?
            .filter(|field| field.name() == Some("map"))
            .ok_or_else(|| FileUploadError::MissingMapField)?
            .bytes()
            .await?;

        let map_field: MapFieldRaw =
            serde_json::from_slice(&bytes).map_err(FileUploadError::InvalidJsonInMapField)?;

        let limit = state.limits.max_files;
        if map_field.len() > limit {
            state.max_files_exceeded = true;
            return Err(FileUploadError::MaxFilesLimitExceeded(limit));
        }
        MapField::new(map_field)
    }

    pub(super) async fn subgraph_stream<FilePrefixFn>(
        &mut self,
        file_names: HashSet<String>,
        file_prefix_fn: FilePrefixFn,
    ) -> SubgraphFileProxyStream<FilePrefixFn>
    where
        FilePrefixFn: Fn(&HeaderMap) -> Bytes,
    {
        let state = self.state.clone().lock_owned().await;
        SubgraphFileProxyStream::new(state, file_names, file_prefix_fn)
    }
}

pin_project! {
    pub(super) struct SubgraphFileProxyStream<FilePrefixFn> {
        state: OwnedMutexGuard<MultipartRequestState>,
        file_names: HashSet<String>,
        file_prefix_fn: FilePrefixFn,
        #[pin]
        current_field: Option<multer::Field<'static>>,
        current_field_bytes: usize,
    }
}

impl<FilePrefixFn> SubgraphFileProxyStream<FilePrefixFn>
where
    FilePrefixFn: Fn(&HeaderMap) -> Bytes,
{
    fn new(
        state: OwnedMutexGuard<MultipartRequestState>,
        file_names: HashSet<String>,
        file_prefix_fn: FilePrefixFn,
    ) -> Self {
        Self {
            state,
            file_names,
            file_prefix_fn,
            current_field: None,
            current_field_bytes: 0,
        }
    }

    fn poll_current_field(
        &mut self,
        cx: &mut task::Context<'_>,
    ) -> Poll<Option<UploadResult<Bytes>>> {
        if let Some(field) = &mut self.current_field {
            let filename = field
                .file_name()
                .or_else(|| field.name())
                .map(|name| format!("'{name}'"))
                .unwrap_or_else(|| "unknown".to_owned());

            let field = Pin::new(field);
            match field.poll_next(cx) {
                Poll::Pending => Poll::Pending,
                Poll::Ready(None) => {
                    self.current_field = None;
                    let file_size = self.current_field_bytes;
                    self.state.file_sizes.push(file_size);
                    Poll::Ready(None)
                }
                Poll::Ready(Some(Ok(bytes))) => {
                    self.current_field_bytes += bytes.len();
                    let limit = self.state.limits.max_file_size;
                    if self.current_field_bytes > (limit.as_u64() as usize) {
                        self.current_field = None;
                        self.state.max_files_size_exceeded = true;
                        Poll::Ready(Some(Err(FileUploadError::MaxFileSizeLimitExceeded {
                            limit,
                            filename,
                        })))
                    } else {
                        Poll::Ready(Some(Ok(bytes)))
                    }
                }
                Poll::Ready(Some(Err(e))) => {
                    Poll::Ready(Some(Err(FileUploadError::InvalidMultipartRequest(e))))
                }
            }
        } else {
            Poll::Ready(None)
        }
    }

    fn poll_next_field(
        mut self: Pin<&mut Self>,
        cx: &mut task::Context<'_>,
    ) -> Poll<Option<UploadResult<Bytes>>> {
        if self.file_names.is_empty() {
            return Poll::Ready(None);
        }
        loop {
            match self.state.multer.poll_next_field(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Ok(None)) => {
                    if self.file_names.is_empty() {
                        return Poll::Ready(None);
                    }

                    let files = mem::take(&mut self.file_names);
                    return Poll::Ready(Some(Err(FileUploadError::MissingFiles(
                        files.into_iter().map(|file| format!("'{file}'")).join(", "),
                    ))));
                }
                Poll::Ready(Ok(Some(field))) => {
                    let limit = self.state.limits.max_files;
                    if self.state.read_files_counter == limit {
                        self.state.max_files_exceeded = true;
                        return Poll::Ready(Some(Err(FileUploadError::MaxFilesLimitExceeded(
                            limit,
                        ))));
                    } else {
                        self.state.read_files_counter += 1;

                        if let Some(name) = field.name()
                            && self.file_names.remove(name)
                        {
                            let prefix = (self.file_prefix_fn)(field.headers());
                            self.current_field = Some(field);
                            return Poll::Ready(Some(Ok(prefix)));
                        }

                        // The file is extraneous, but the rest can still be processed.
                        // Just ignore it and don’t exit with an error.
                        // Matching https://github.com/jaydenseric/graphql-upload/blob/f24d71bfe5be343e65d084d23073c3686a7f4d18/processRequest.mjs#L231-L236
                    }
                }
                Poll::Ready(Err(e)) => return Poll::Ready(Some(Err(e.into()))),
            }
        }
    }
}

impl<FilePrefixFn> Stream for SubgraphFileProxyStream<FilePrefixFn>
where
    FilePrefixFn: Fn(&HeaderMap) -> Bytes,
{
    type Item = UploadResult<Bytes>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut task::Context<'_>) -> Poll<Option<Self::Item>> {
        let field_result = self.poll_current_field(cx);
        match field_result {
            Poll::Ready(None) => self.poll_next_field(cx),
            _ => field_result,
        }
    }
}

#[cfg(test)]
mod tests {
    use bytesize::ByteSize;

    use super::*;
    use crate::plugins::limits::RouterLimitsConfig;

    /// The value the router passes as `operations_size_limit` for a default configuration.
    fn default_operations_size_limit() -> u64 {
        RouterLimitsConfig::default().http_max_request_bytes as u64
    }

    #[test]
    fn whole_stream_budget_sums_the_configured_limits() {
        let limits = MultipartRequestLimits::default();

        assert_eq!(
            whole_stream_size_limit(&limits, default_operations_size_limit()),
            // http_max_request_bytes + map cap + max_files * max_file_size + max_overhead_size
            default_operations_size_limit() + MAP_SIZE_LIMIT + 5 * 1_000_000 + 2_000_000,
        );
    }

    /// An absurd configuration must degrade to "effectively unlimited" rather than panicking in
    /// debug or wrapping around to a tiny budget in release. `u64::MAX` is not a sentinel in multer
    /// — it is just a threshold `bytes_pulled > limit` can never cross — so this is a documented
    /// loss of enforcement, not a special case.
    #[test]
    fn whole_stream_budget_saturates_instead_of_overflowing() {
        let limits = MultipartRequestLimits {
            max_files: usize::MAX,
            max_file_size: ByteSize::b(u64::MAX),
            ..MultipartRequestLimits::default()
        };

        assert_eq!(whole_stream_size_limit(&limits, u64::MAX), u64::MAX);
    }

    /// A large-but-real configuration must not saturate, or the limit silently stops applying.
    #[test]
    fn whole_stream_budget_does_not_saturate_for_realistic_large_limits() {
        // Matches tests/fixtures/file_upload/large.router.yaml.
        let limits = MultipartRequestLimits {
            max_files: 10,
            max_file_size: ByteSize::gb(15),
            ..MultipartRequestLimits::default()
        };

        let budget = whole_stream_size_limit(&limits, default_operations_size_limit());
        assert!(budget < u64::MAX);
        assert!(budget > 150_000_000_000);
    }
}
