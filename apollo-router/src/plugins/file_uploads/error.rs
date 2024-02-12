use crate::graphql;
use thiserror::Error;

/// Errors that may occur during file upload
#[derive(Debug, Error)]
pub(crate) enum FileUploadError {
    /// Represents an invalid request, wrapping the context as a string
    #[error("invalid multipart request: {0}")]
    InvalidMultipartRequest(#[from] multer::Error),
}

impl From<FileUploadError> for graphql::Error {
    fn from(value: FileUploadError) -> Self {
        Self::builder()
            .message(value.to_string())
            .extension_code("FILE_UPLOAD") // FIXME: Figure out what this should be
            .build()
    }
}
