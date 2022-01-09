use ::tracing::{
    field::{Field, Visit},
    span,
};
use serde::{Deserialize, Serialize};

use crate::attribute::OwnedMetadata;

type FieldName = String;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(untagged)]
pub enum RecordValue {
    Error(String),
    Value(serde_json::Value),
    Debug(String),
}

#[derive(Default, Clone, Debug)]
pub(crate) struct RecordEverything(Vec<Record>);

impl RecordEverything {
    pub fn contents(&self) -> impl Iterator<Item = &Record> {
        self.0.iter()
    }
}

impl Visit for RecordEverything {
    /// Visit a double-precision floating point value.
    fn record_f64(&mut self, field: &Field, value: f64) {
        self.0
            .push((field.name().to_string(), RecordValue::Value(value.into())));
    }

    /// Visit a signed 64-bit integer value.
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.0
            .push((field.name().to_string(), RecordValue::Value(value.into())));
    }

    /// Visit an unsigned 64-bit integer value.
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.0
            .push((field.name().to_string(), RecordValue::Value(value.into())));
    }

    /// Visit a boolean value.
    fn record_bool(&mut self, field: &Field, value: bool) {
        self.0
            .push((field.name().to_string(), RecordValue::Value(value.into())));
    }

    /// Visit a string value.
    fn record_str(&mut self, field: &Field, value: &str) {
        self.0
            .push((field.name().to_string(), RecordValue::Value(value.into())));
    }

    /// Records a type implementing `Error`.
    ///
    /// <div class="example-wrap" style="display:inline-block">
    /// <pre class="ignore" style="white-space:normal;font:inherit;">
    /// <strong>Note</strong>: This is only enabled when the Rust standard library is
    /// present.
    /// </pre>
    #[cfg(feature = "std")]
    #[cfg_attr(docsrs, doc(cfg(feature = "std")))]
    fn record_error(&mut self, field: &Field, value: &(dyn std::error::Error + 'static)) {
        self.0.push((
            field.name().to_string(),
            RecordValue::Error(&format_args!("{}", value).into()),
        ));
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.0.push((
            field.name().to_string(),
            RecordValue::Debug(format!("{:?}", value)),
        ));
    }
}

pub type Record = (FieldName, RecordValue);

#[derive(Clone, Debug, Default)]
pub struct Recorder {
    metadata: Option<OwnedMetadata>,
    visitor: RecordEverything,
}

impl Recorder {
    pub fn attributes(&mut self, span_id: tracing::Id, attributes: &span::Attributes<'_>) {
        let mut owned_metadata: OwnedMetadata = attributes.metadata().into();
        owned_metadata.span_id = Some(span_id.into_u64());
        self.metadata = Some(owned_metadata);
        attributes.record(&mut self.visitor)
    }

    pub fn metadata(&self) -> Option<&OwnedMetadata> {
        self.metadata.as_ref()
    }

    pub fn record(&mut self, record: &span::Record<'_>) {
        record.record(&mut self.visitor)
    }

    pub fn contents(&self, level: &tracing::Level) -> RecordWithMetadata {
        let mut r = RecordWithMetadata::new(self.metadata.clone().unwrap());

        if &r
            .metadata
            .level
            .clone()
            .parse::<tracing::Level>()
            .expect("invalid level")
            <= level
        {
            r.append(self.visitor.0.clone());
        }
        r
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RecordWithMetadata {
    entries: Vec<Record>,
    metadata: OwnedMetadata,
}

impl RecordWithMetadata {
    pub fn new(metadata: OwnedMetadata) -> Self {
        Self {
            entries: Default::default(),
            metadata,
        }
    }

    pub fn metadata(&self) -> OwnedMetadata {
        self.metadata.clone()
    }

    pub fn for_root() -> Self {
        Self {
            entries: Vec::new(),
            metadata: OwnedMetadata {
                name: "root".to_string(),
                ..Default::default()
            },
        }
    }

    pub fn entries(&self) -> impl Iterator<Item = &Record> {
        self.entries.iter()
    }

    pub fn push(&mut self, entry: Record) {
        self.entries.push(entry)
    }

    pub fn append(&mut self, mut entries: Vec<Record>) {
        self.entries.append(&mut entries)
    }
}
