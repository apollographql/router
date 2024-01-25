use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct RestrictedMultipartRequestLimits {
    pub(crate) max_files: usize,
    pub(crate) max_file_size: usize,
}

impl Default for RestrictedMultipartRequestLimits {
    fn default() -> Self {
        Self {
            max_files: 5,
            max_file_size: 5_242_820, // 5mb
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct RestrictedMultipartRequest {
    pub(crate) limits: RestrictedMultipartRequestLimits,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct FileUploadProtocols {
    pub(crate) restricted_multipart_request: RestrictedMultipartRequest,
}

/// Configuration for File Uploads
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct FileUploadsConfig {
    /// Protocols enable for file upload
    pub(crate) protocols: FileUploadProtocols,
}
