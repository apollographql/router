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
        self.print("experimental", &self.experimental)
    }

    pub(crate) fn print_preview(&self) {
        self.print("preview", &self.preview)
    }

    pub(crate) fn print(&self, stage: &str, urls: &HashMap<String, String>) {
        let mut list: Vec<_> = urls
            .iter()
            .map(|(config_key, discussion_link)| format!("\t- {config_key}: {discussion_link}"))
            .collect();
        if list.is_empty() {
            println!("This Router version has no {stage} configuration")
        } else {
            list.sort();
            let list = list.join("\n");
            println!(
                "List of all {stage} configurations with related GitHub discussions:\n\n{list}"
            )
        }
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
        let mut list: Vec<_> = used
            .into_iter()
            .filter_map(|config_key| {
                urls.get(&config_key)
                    .map(|discussion_link| format!("\t- {config_key}: {discussion_link}"))
            })
            .collect();
        if !list.is_empty() {
            list.sort();
            let list = list.join("\n");
            tracing::info!(
                "You're using some \"{prefix}\" features of the Apollo Router \
                (those which have their configuration prefixed by \"{prefix}_\").\n\
                {stage_description}\n\
                Here is a list of links where you can give your opinion:\n\n\
                {list}\n\n\
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
            },
            "preview_subscription": {

            }
        });

        assert_eq!(
            get_configurations(&val, "experimental"),
            vec![
                "experimental_logging".to_string(),
                "experimental_trace_id".to_string()
            ]
        );
        assert_eq!(
            get_configurations(&val, "preview"),
            vec!["preview_subscription".to_string(),]
        );
    }
}
