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

    #[error("Missing files in the request: {0}.")]
    MissingFiles(String),

    #[error("Variables containing files are forbidden inside @defer: {0}.")]
    VariblesForbiddenInsideDeffer(String),

    #[error("Variables containing files are forbidden inside subscription: {0}.")]
    VariblesForbiddenInsideSubscription(String),

    #[error("References to variables containing files are ordered in the way that prevent streaming of files.")]
    MisorderedVariables,

    #[error("Variables use mutiple time in the way that prevent streaming of files: {0}.")]
    DuplicateVariableUsages(String),

    #[error("Exceeded the limit of {0} file uploads of files in a single request.")]
    MaxFilesLimitExceeded(usize),

    #[error("Exceeded the limit of {limit} bytes on {file_name} file.")]
    MaxFileSizeLimitExceded { limit: u64, file_name: String },

    #[error("{0}")]
    HyperBodyErrorWrapper(#[from] hyper::Error),
}

impl From<FileUploadError> for graphql::Error {
    fn from(value: FileUploadError) -> Self {
        Self::builder()
            .message(value.to_string())
            .extension_code("FILE_UPLOAD") // FIXME: Figure out what this should be
            .build()
    }
}
