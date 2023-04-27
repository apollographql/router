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
        self.log_used(
            conf,
            "experimental",
            &self.experimental,
            "We may make breaking changes in future releases. \
            To help us design the stable version we need your feedback.",
        )
    }

    pub(crate) fn log_preview_used(&self, conf: &Value) {
        self.log_used(
            conf,
            "preview",
            &self.preview,
            "These features are not officially supported with any SLA \
            and may still contain bugs or undergo iteration. \
            You're encouraged to try preview features in test environments \
            to familiarize yourself with upcoming functionality \
            before it reaches general availability.",
        )
    }

    fn log_used(
        &self,
        conf: &Value,
        prefix: &str,
        urls: &HashMap<String, String>,
        stage_description: &str,
    ) {
        let used = get_configurations(conf, &format!("{prefix}_"));
        let needed_discussions = used
            .into_iter()
            .filter_map(|config_key| {
                urls.get(&config_key)
                    .map(|discussion_link| format!("\t- {config_key}: {discussion_link}"))
            })
            .collect::<Vec<String>>()
            .join("\n");

        if !needed_discussions.is_empty() {
            tracing::info!(
                "You're using some \"{prefix}\" features of the Apollo Router \
                (those which have their configuration prefixed by \"{prefix}_\"). \
                {stage_description} \
                Here is a list of links where you can give your opinion:

                {needed_discussions}

                For more information about launch stages, please see the documentation here: \
                https://www.apollographql.com/docs/resources/product-launch-stages/",
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
            if field_name.starts_with(prefix) {
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
