use std::collections::HashMap;

use serde_json::Value;

pub(crate) fn print_all_experimental_conf() {
    let available_exp_confs = serde_json::from_str::<HashMap<String, String>>(include_str!(
        "../../experimental_features.json"
    ))
    .expect("cannot load the list of available experimental configurations");

    let available_exp_confs_str: Vec<String> = available_exp_confs
        .into_iter()
        .map(|(used_exp_conf, discussion_link)| format!("\t- {used_exp_conf}: {discussion_link}"))
        .collect();
    println!(
        "List of all experimental configurations with related GitHub discussions:\n\n{}",
        available_exp_confs_str.join("\n")
    );
}

pub(crate) fn log_used_experimental_conf(conf: &Value) {
    let available_discussions = serde_json::from_str::<HashMap<String, String>>(include_str!(
        "../../experimental_features.json"
    ));
    if let Ok(available_discussions) = available_discussions {
        let used_experimental_conf = get_experimental_configurations(conf);
        let needed_discussions: Vec<String> = used_experimental_conf
            .into_iter()
            .filter_map(|used_exp_conf| {
                available_discussions
                    .get(&used_exp_conf)
                    .map(|discussion_link| format!("\t- {used_exp_conf}: {discussion_link}"))
            })
            .collect();
        if !needed_discussions.is_empty() {
            tracing::info!(
                r#"You're using some "experimental" features (configuration prefixed by "experimental_" or contained within an "experimental" section), we may make breaking changes in future releases.
To help us design the stable version we need your feedback, here is a list of links where you can give your opinion:

{}
"#,
                needed_discussions.join("\n")
            );
        }
    }
}

fn get_experimental_configurations(conf: &Value) -> Vec<String> {
    let mut experimental_fields = Vec::new();
    visit_experimental_configurations(conf, &mut experimental_fields);

    experimental_fields
}

pub(crate) fn visit_experimental_configurations(
    conf: &Value,
    experimental_fields: &mut Vec<String>,
) {
    if let Value::Object(object) = conf {
        object.iter().for_each(|(field_name, val)| {
            if field_name.starts_with("experimental_") {
                experimental_fields.push(field_name.clone());
            }
            // TODO: Remove when JWT authentication is generally available
            if field_name == "experimental" {
                experimental_fields.push("experimental_jwt_authentication".to_string());
            }
            visit_experimental_configurations(val, experimental_fields);
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
            get_experimental_configurations(&val),
            vec![
                "experimental_logging".to_string(),
                "experimental_trace_id".to_string()
            ]
        );
    }
}
