use serde::{Deserialize, Serialize};
use tracing::{Level, Metadata};

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq, Hash)]

pub struct OwnedFieldSet {
    names: Vec<String>,
}

impl From<&tracing::field::FieldSet> for OwnedFieldSet {
    fn from(fs: &tracing::field::FieldSet) -> Self {
        Self {
            names: fs.iter().map(|field| field.to_string()).collect(),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct OwnedMetadata {
    pub name: String,

    /// The part of the system that the span that this metadata describes
    /// occurred in.
    pub target: String,

    /// The level of verbosity of the described span.
    // TODO[igni]: maybe put an enum here
    pub level: String,

    /// The name of the Rust module where the span occurred, or `None` if this
    /// could not be determined.
    pub module_path: Option<String>,

    /// The name of the source code file where the span occurred, or `None` if
    /// this could not be determined.
    #[serde(skip_serializing)]
    pub file: Option<String>,

    /// The line number in the source code file where the span occurred, or
    /// `None` if this could not be determined.
    #[serde(skip_serializing)]
    pub line: Option<u32>,

    /// The names of the key-value fields attached to the described span or
    /// event.
    pub fields: OwnedFieldSet,

    #[serde(skip_serializing)]
    /// span_id when available, used to match logs and spans when applicable
    pub span_id: Option<u64>,
}

impl From<&Metadata<'_>> for OwnedMetadata {
    fn from(md: &Metadata) -> Self {
        Self {
            name: md.name().to_string(),
            target: md.target().to_string(),
            level: md.level().to_string(),
            module_path: md.module_path().map(std::string::ToString::to_string),
            file: md.file().map(std::string::ToString::to_string),
            line: md.line(),
            fields: md.fields().into(),
            span_id: None,
        }
    }
}

impl OwnedMetadata {
    pub fn with_span_id(self, span_id: u64) -> Self {
        Self {
            span_id: Some(span_id),
            ..self
        }
    }

    pub fn is_enabled(&self, max_verbosity_level: &Level) -> bool {
        &self.level.parse::<tracing::Level>().unwrap_or(Level::INFO) <= max_verbosity_level
    }
}
