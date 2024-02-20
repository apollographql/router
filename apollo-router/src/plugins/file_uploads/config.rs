use bytesize::ByteSize;
use schemars::JsonSchema;
use serde::Deserialize;

/// Request limits for a multipart request
#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct MultipartRequestLimits {
    /// The maximum amount of files allowed for a single query (default: 4)
    pub(crate) max_files: usize,

    /// The maximum size of each file, in bytes (default: 5MB)
    #[serde(deserialize_with = "bytesize::ByteSize::deserialize")]
    #[schemars(with = "String")]
    pub(crate) max_file_size: ByteSize,
}

impl Default for MultipartRequestLimits {
    fn default() -> Self {
        Self {
            max_files: 5,
            max_file_size: ByteSize::mb(1),
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
