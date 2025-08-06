use serde::Deserialize;
use serde::Serialize;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ProblemLocation {
    RequestBody,
    SourceUrl,
    SourcePath,
    SourceQueryParams,
    ConnectUrl,
    ConnectPath,
    ConnectQueryParams,
    SourceHeaders,
    ConnectHeaders,
    IsSuccess,
    Selection,
    ErrorsMessage,
    SourceErrorsExtensions,
    ConnectErrorsExtensions,
}
