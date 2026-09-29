use std::time::Duration;

use bytesize::ByteSize;
use schemars::JsonSchema;
use serde::Deserialize;

/// Request limits for a multipart request
#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct MultipartRequestLimits {
    /// The maximum amount of files allowed for a single query (default: 5)
    pub(crate) max_files: usize,

    /// The maximum size of each file (default: 1MB)
    #[serde(deserialize_with = "bytesize::ByteSize::deserialize")]
    #[schemars(with = "String")]
    pub(crate) max_file_size: ByteSize,

    /// Maximum time allowed for the client to deliver the GraphQL operation (query and variables)
    /// within the multipart body. This timeout does not apply to reading the file contents
    /// themselves. If the operation part of the request is too slow to arrive, the request is
    /// rejected with a `504 Gateway Timeout` error.
    ///
    /// If not set, no operation body timeout is applied.
    #[serde(deserialize_with = "humantime_serde::deserialize", default)]
    #[schemars(with = "Option<String>", default)]
    pub(crate) operation_body_timeout: Option<Duration>,

    /// The maximum amount of multipart framing — the preamble, part headers, boundary delimiters,
    /// and transport padding — permitted in a single upload request.
    ///
    /// The parser cannot tell a framing byte from a content byte. The router therefore adds this
    /// allowance to the content the other limits already permit. It enforces the sum as a limit on
    /// the whole request. A request over that sum gets a `413 Payload Too Large`. The sum grows
    /// with `max_files` and `max_file_size`.
    #[serde(
        deserialize_with = "bytesize::ByteSize::deserialize",
        default = "default_max_overhead_size"
    )]
    #[schemars(with = "String", default = "default_max_overhead_size")]
    pub(crate) max_overhead_size: ByteSize,
}

/// Deliberately generous. The whole-request budget this allowance feeds and `max_file_size` count
/// bytes at different points:
///
/// - multer counts every byte as it arrives off the socket, before it parses anything.
/// - `max_file_size` counts a file's bytes as the router hands them to the subgraph.
///
/// The first count leads the second by whatever multer holds in its buffer. A single read of the
/// connection can deliver ~400 KB, the default for `limits.router.http1_max_request_buf_size`.
/// multer keeps reading until the body gives it nothing, so the gap can reach several times that.
///
/// With a budget that small, one oversized file breaks the whole-request budget before it breaks
/// `max_file_size`. The client gets a "request too large" error instead of a "max file size
/// exceeded" error. A megabyte-scale allowance keeps the budget clear of the gap.
fn default_max_overhead_size() -> ByteSize {
    ByteSize::mb(2)
}

impl Default for MultipartRequestLimits {
    fn default() -> Self {
        Self {
            max_files: 5,
            max_file_size: ByteSize::mb(1),
            operation_body_timeout: None,
            max_overhead_size: default_max_overhead_size(),
        }
    }
}

/// Supported mode for a multipart request
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "lowercase")]
pub(crate) enum MultipartRequestMode {
    /// The multipart request will not be loaded into memory and instead will
    /// be streamed directly to the subgraph in the order received. This has some
    /// limitations, mainly that the query _must_ be able to be streamed directly
    /// to the subgraph without buffering.
    ///
    /// In practice, this means that certain queries will fail due to ordering of the
    /// files.
    #[default]
    Stream,
}

/// Configuration for a multipart request for file uploads.
///
/// This protocol conforms to [jaydenseric's multipart spec](https://github.com/jaydenseric/graphql-multipart-request-spec)
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct MultipartRequest {
    /// Whether to enable the multipart protocol for file uploads (default: true)
    pub(crate) enabled: bool,

    /// The supported mode for the request (default: [MultipartRequestMode::Stream])
    pub(crate) mode: MultipartRequestMode,

    /// Resource limits for multipart requests
    pub(crate) limits: MultipartRequestLimits,
}

impl Default for MultipartRequest {
    fn default() -> Self {
        Self {
            enabled: true,
            mode: Default::default(),
            limits: Default::default(),
        }
    }
}

/// Configuration for the various protocols supported by the file upload plugin
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct FileUploadProtocols {
    /// Configuration for multipart requests.
    ///
    /// This protocol conforms to [jaydenseric's multipart spec](https://github.com/jaydenseric/graphql-multipart-request-spec)
    pub(crate) multipart: MultipartRequest,
}

/// Configuration for File Uploads plugin
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct FileUploadsConfig {
    /// Whether the file upload plugin should be enabled (default: false)
    pub(crate) enabled: bool,

    /// Supported protocol configurations for file uploads
    pub(crate) protocols: FileUploadProtocols,
}
