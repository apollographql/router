use serde::Deserialize;
use serde::Serialize;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
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
