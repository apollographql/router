use itertools::Itertools;
use levenshtein::levenshtein;

use crate::utils::human_readable;

pub(crate) fn suggestion_list(
    input: &str,
    options: impl IntoIterator<Item = String>,
) -> Vec<String> {
    let threshold = 1 + (input.len() as f64 * 0.4).floor() as usize;
    let input_lowercase = input.to_lowercase();
    let mut result = Vec::new();
    for option in options {
        // Special casing so that if the only mismatch is in upper/lower-case, then the option is
        // always shown.
        let distance = if input_lowercase == option.to_lowercase() {
            1
        } else {
            levenshtein(input, &option)
        };
        if distance <= threshold {
            result.push((option, distance));
        }
    }
    result.sort_by_key(|x| x.1);
    result.into_iter().map(|(s, _)| s.to_string()).collect()
}

const MAX_SUGGESTIONS: usize = 5;

/// Given [ A, B ], returns "Did you mean A or B?".
/// Given [ A, B, C ], returns "Did you mean A, B, or C?".
pub(crate) fn did_you_mean(suggestions: impl IntoIterator<Item = String>) -> String {
    const MESSAGE: &str = " Did you mean ";
    let suggestions = suggestions
        .into_iter()
        .take(MAX_SUGGESTIONS)
        .map(|s| format!("\"{s}\""))
        .collect_vec();
    if suggestions.is_empty() {
        return String::new();
    }
    let last_separator = if suggestions.len() > 2 {
        Some(", or ")
    } else {
        Some(" or ")
    };
    let suggestion_str = human_readable::join_strings(
        suggestions.iter(),
        human_readable::JoinStringsOptions {
            separator: ", ",
            first_separator: None,
            last_separator,
            output_length_limit: None,
        },
    );
    format!("{MESSAGE}{suggestion_str}?")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn did_you_mean_returns_empty_string_for_no_suggestions() {
        assert_eq!(did_you_mean(Vec::<String>::new()), "");
    }

    #[test]
    fn did_you_mean_with_one_suggestion() {
        assert_eq!(
            did_you_mean(vec!["foo".to_string()]),
            r#" Did you mean "foo"?"#
        );
    }

    #[test]
    fn did_you_mean_with_two_suggestions() {
        assert_eq!(
            did_you_mean(vec!["foo".to_string(), "bar".to_string()]),
            r#" Did you mean "foo" or "bar"?"#
        );
    }

    #[test]
    fn did_you_mean_with_three_suggestions() {
        assert_eq!(
            did_you_mean(vec![
                "foo".to_string(),
                "bar".to_string(),
                "baz".to_string()
            ]),
            r#" Did you mean "foo", "bar", or "baz"?"#
        );
    }
}
