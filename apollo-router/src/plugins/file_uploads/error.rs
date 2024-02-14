use thiserror::Error;

use crate::graphql;

/// Errors that may occur during file upload
#[derive(Debug, Error)]
pub(crate) enum FileUploadError {
    /// Represents an invalid request, wrapping the context as a string
    #[error("invalid multipart request: {0}")]
    InvalidMultipartRequest(#[from] multer::Error),

    #[error("Missing multipart field 'operations', it should be a first field in request body.")]
    MissingOperationsField,

    #[error("Missing multipart field 'map', it should be a second field in request body.")]
    MissingMapField,

    #[error("Invalid JSON in the ‘map’ multipart field: {0}")]
    InvalidJsonInMapField(serde_json::Error),

    #[error("Batched requests are not supported for file uploads.")]
    BatchRequestAreNotSupported,

    #[error("Invalid path '{0}' found inside 'map' field, it should start with 'variables.'.")]
    InvalidPathInsideMapField(String),

    #[error("Invalid path '{0}' found inside 'map' field, missing name of variable.")]
    MissingVariableNameInsideMapField(String),

    #[error("Invalid path '{0}' found inside 'map' field, it does not point to a valid value inside 'operations' field.")]
    InputValueNotFound(String),

    // FIXME: better name
    #[error("missing files.")]
    FilesMissing,
}

impl From<FileUploadError> for graphql::Error {
    fn from(value: FileUploadError) -> Self {
        Self::builder()
            .message(value.to_string())
            .extension_code("FILE_UPLOAD") // FIXME: Figure out what this should be
            .build()
    }
}
