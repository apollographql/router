use std::collections::HashMap;

use crate::graphql::ResponseVisitor;
use crate::plugins::telemetry::metrics::apollo::list_length_histogram::ListLengthHistogram;

pub(crate) struct FieldLengthRecorder {
    // Maps type -> field -> histogram (of lengths)
    pub(crate) field_lengths: HashMap<String, HashMap<String, ListLengthHistogram>>,
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
                self.field_lengths
                    .entry(ty.to_string())
                    .or_default()
                    .entry(field.name.to_string())
                    .or_default()
                    .record(items.len());

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
