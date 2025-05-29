use serde::Deserialize;
use serde::Serialize;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum ProblemLocation {
    RequestBody,
    SourceUri,
    SourcePath,
    SourceQueryParams,
    ConnectUri,
    ConnectPath,
    ConnectQueryParams,
    SourceHeaders,
    ConnectHeaders,
    Selection,
    ErrorsMessage,
    SourceErrorsExtensions,
    ConnectErrorsExtensions,
}
