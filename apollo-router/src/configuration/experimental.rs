use std::collections::HashMap;

use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
pub(crate) struct Discussed {
    experimental: HashMap<String, String>,
    preview: HashMap<String, String>,
}

impl Discussed {
    pub(crate) fn new() -> Self {
        serde_json::from_str(include_str!("../../feature_discussions.json"))
            .expect("cannot load the list of available discussed configurations")
    }

    pub(crate) fn print_experimental(&self) {
        let available_exp_confs_str: Vec<String> = self
            .experimental
            .iter()
            .map(|(used_exp_conf, discussion_link)| {
                format!("\t- {used_exp_conf}: {discussion_link}")
            })
            .collect();
        println!(
            "List of all experimental configurations with related GitHub discussions:\n\n{}",
            available_exp_confs_str.join("\n")
        );
    }

    pub(crate) fn print_preview(&self) {
        let available_confs_str: Vec<String> = self
            .preview
            .iter()
            .map(|(used_conf, discussion_link)| format!("\t- {used_conf}: {discussion_link}"))
            .collect();
        println!(
            "List of all preview configurations with related GitHub discussions:\n\n{}",
            available_confs_str.join("\n")
        );
    }

    pub(crate) fn log_experimental_used(&self, conf: &Value) {
        let used_experimental_conf = get_configurations(conf, "experimental");

        let needed_discussions: Vec<String> = used_experimental_conf
            .into_iter()
            .filter_map(|used_exp_conf| {
                self.experimental
                    .get(&used_exp_conf)
                    .map(|discussion_link| format!("\t- {used_exp_conf}: {discussion_link}"))
            })
            .collect();
        if !needed_discussions.is_empty() {
            tracing::info!(
                r#"You're using some "experimental" features (configuration prefixed by "experimental_" or contained within an "experimental" section). We may make breaking changes in future releases.
To help us design the stable version we need your feedback. Here is a list of links where you can give your opinion:

{}
"#,
                needed_discussions.join("\n")
            );
        }
    }

    pub(crate) fn log_preview_used(&self, conf: &Value) {
        let used_preview_conf = get_configurations(conf, "preview");

        let needed_discussions: Vec<String> = used_preview_conf
            .into_iter()
            .filter_map(|used_conf| {
                self.experimental
                    .get(&used_conf)
                    .map(|discussion_link| format!("\t- {used_conf}: {discussion_link}"))
            })
            .collect();
        if !needed_discussions.is_empty() {
            tracing::info!(
                r#"You're using some "preview" features of the Apollo Router (those which have their configuration prefixed by "preview").  These features are not officially supported with any SLA and may still contain bugs or undergo iteration. You're encouraged to try preview features in test environments to familiarize yourself with upcoming functionality before it reaches general availability. For more information about launch stages, please see the documentation here: https://www.apollographql.com/docs/resources/product-launch-stages/"#,
            );
        }
    }
}

fn get_configurations(conf: &Value, prefix: &str) -> Vec<String> {
    let mut fields = Vec::new();
    visit_configurations(conf, prefix, &mut fields);
    fields
}

fn visit_configurations(conf: &Value, prefix: &str, fields: &mut Vec<String>) {
    if let Value::Object(object) = conf {
        object.iter().for_each(|(field_name, val)| {
            if field_name.starts_with(&format!("{}_", prefix)) {
                fields.push(field_name.clone());
            }
            visit_configurations(val, prefix, fields);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_experimental_configurations() {
        let val = serde_json::json!({
            "server": "test",
            "experimental_logging": {
                "value": "foo",
                "sub": {
                    "experimental_trace_id": "ok"
                }
            }
        });

        assert_eq!(
            get_configurations(&val, "experimental"),
            vec![
                "experimental_logging".to_string(),
                "experimental_trace_id".to_string()
            ]
        );
    }
}
