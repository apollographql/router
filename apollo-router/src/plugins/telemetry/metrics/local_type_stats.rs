use std::collections::HashMap;

use crate::graphql::ResponseVisitor;
use crate::json_ext::Object;
use crate::plugins::telemetry::metrics::apollo::histogram::ListLengthHistogram;
use crate::plugins::telemetry::metrics::apollo::studio::LocalFieldStat;
use crate::plugins::telemetry::metrics::apollo::studio::LocalTypeStat;

#[derive(Clone, Debug, Default)]
pub(crate) struct LocalTypeStatRecorder {
    pub(crate) local_type_stats: HashMap<String, LocalTypeStat>,
}

impl LocalTypeStatRecorder {
    pub(crate) fn new() -> Self {
        Self {
            local_type_stats: Default::default(),
        }
    }
}

impl ResponseVisitor for LocalTypeStatRecorder {
    fn visit_field(
        &mut self,
        request: &apollo_compiler::ExecutableDocument,
        variables: &Object,
        ty: &apollo_compiler::executable::NamedType,
        field: &apollo_compiler::executable::Field,
        value: &serde_json_bytes::Value,
    ) {
        match value {
            serde_json_bytes::Value::Array(items) => {
                let stat = match self.local_type_stats.get_mut(ty.as_str()) {
                    Some(map) => map,
                    None => {
                        self.local_type_stats
                            .insert(ty.to_string(), LocalTypeStat::default());
                        self.local_type_stats
                            .get_mut(ty.as_str())
                            .expect("already inserted")
                    }
                };

                let local_field_stat = match stat.local_per_field_stat.get_mut(field.name.as_str())
                {
                    Some(st) => st,
                    None => {
                        stat.local_per_field_stat.insert(
                            field.name.to_string(),
                            LocalFieldStat {
                                return_type: field.ty().inner_named_type().to_string(),
                                list_lengths: ListLengthHistogram::new(None),
                            },
                        );
                        stat.local_per_field_stat
                            .get_mut(field.name.as_str())
                            .expect("already inserted")
                    }
                };

                local_field_stat
                    .list_lengths
                    .record(Some(items.len() as u64), 1);

                for item in items {
                    self.visit_list_item(
                        request,
                        variables,
                        field.ty().inner_named_type(),
                        field,
                        item,
                    );
                }
            }
            serde_json_bytes::Value::Object(children) => {
                self.visit_selections(request, variables, &field.selection_set, children);
            }
            _ => {}
        }
    }
}
