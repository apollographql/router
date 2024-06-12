use std::collections::HashMap;

use hdrhistogram::Histogram;

use crate::response::ResponseVisitor;

pub(crate) struct FieldLengthRecorder {
    // Maps type -> field -> histogram (of lengths)
    pub(crate) field_lengths: HashMap<String, HashMap<String, Histogram<u64>>>,
}

impl FieldLengthRecorder {
    pub(crate) fn new() -> Self {
        Self {
            field_lengths: Default::default(),
        }
    }
}

impl ResponseVisitor for FieldLengthRecorder {
    fn visit_field(
        &mut self,
        request: &apollo_compiler::ExecutableDocument,
        ty: &apollo_compiler::executable::NamedType,
        field: &apollo_compiler::executable::Field,
        value: &serde_json_bytes::Value,
    ) {
        match value {
            serde_json_bytes::Value::Array(items) => {
                if self
                    .field_lengths
                    .entry(ty.to_string())
                    .or_default()
                    .entry(field.name.to_string())
                    .or_insert_with(|| Histogram::new(3).expect("Histogram can be created"))
                    .record(items.len() as u64)
                    .is_err()
                {
                    tracing::warn!("failed to record field length in histogram")
                }

                for item in items {
                    self.visit_list_item(request, field.ty().inner_named_type(), field, item);
                }
            }
            serde_json_bytes::Value::Object(children) => {
                self.visit_selections(request, &field.selection_set, children);
            }
            _ => {}
        }
    }
}
