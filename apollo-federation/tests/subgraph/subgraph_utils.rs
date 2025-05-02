use apollo_federation::subgraph::SubgraphError;
use apollo_federation::subgraph::typestate::Subgraph;
use apollo_federation::subgraph::typestate::Validated;

pub(crate) enum BuildOption {
    AsIs,
    AsFed2,
}

pub(crate) fn build_inner(
    schema_str: &str,
    build_option: BuildOption,
) -> Result<Subgraph<Validated>, SubgraphError> {
    let name = "S";
    let subgraph =
        Subgraph::parse(name, &format!("http://{name}"), schema_str).expect("valid schema");
    let subgraph = if matches!(build_option, BuildOption::AsFed2) {
        subgraph
            .into_fed2_subgraph()
            .map_err(|e| SubgraphError::new(name, e))?
    } else {
        subgraph
    };
    subgraph
        .expand_links()
        .map_err(|e| SubgraphError::new(name, e))?
        .validate(true)
}

pub(crate) fn build_and_validate(schema_str: &str) -> Subgraph<Validated> {
    build_inner(schema_str, BuildOption::AsIs).expect("expanded subgraph to be valid")
}

pub(crate) fn build_for_errors_with_option(
    schema: &str,
    build_option: BuildOption,
) -> Vec<(String, String)> {
    build_inner(schema, build_option)
        .expect_err("subgraph error was expected")
        .format_errors()
}

/// Build subgraph expecting errors, assuming fed 2.
pub(crate) fn build_for_errors(schema: &str) -> Vec<(String, String)> {
    build_for_errors_with_option(schema, BuildOption::AsFed2)
}

pub(crate) fn remove_indentation(s: &str) -> String {
    // count the last lines that are space-only
    let first_empty_lines = s.lines().take_while(|line| line.trim().is_empty()).count();
    let last_empty_lines = s
        .lines()
        .rev()
        .take_while(|line| line.trim().is_empty())
        .count();

    // lines without the space-only first/last lines
    let lines = s
        .lines()
        .skip(first_empty_lines)
        .take(s.lines().count() - first_empty_lines - last_empty_lines);

    // compute the indentation
    let indentation = lines
        .clone()
        .map(|line| line.chars().take_while(|c| *c == ' ').count())
        .min()
        .unwrap_or(0);

    // remove the indentation
    lines
        .map(|line| {
            line.trim_end()
                .chars()
                .skip(indentation)
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// True if a and b contain the same error messages
pub(crate) fn check_errors(a: &[(String, String)], b: &[(&str, &str)]) -> Result<(), String> {
    if a.len() != b.len() {
        return Err(format!(
            "Mismatched error counts: {} != {}\n\nexpected:\n{}\n\nactual:\n{}",
            b.len(),
            a.len(),
            b.iter()
                .map(|(code, msg)| { format!("- {}: {}", code, msg) })
                .collect::<Vec<_>>()
                .join("\n"),
            a.iter()
                .map(|(code, msg)| { format!("+ {}: {}", code, msg) })
                .collect::<Vec<_>>()
                .join("\n"),
        ));
    }

    // remove indentations from messages to ignore indentation differences
    let b_iter = b
        .iter()
        .map(|(code, message)| (*code, remove_indentation(message)));
    let diff: Vec<_> = a
        .iter()
        .map(|(code, message)| (code.as_str(), remove_indentation(message)))
        .zip(b_iter)
        .filter(|(a_i, b_i)| a_i.0 != b_i.0 || a_i.1 != b_i.1)
        .collect();
    if diff.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "Mismatched errors:\n{}\n",
            diff.iter()
                .map(|(a_i, b_i)| { format!("- {}: {}\n+ {}: {}", b_i.0, b_i.1, a_i.0, a_i.1) })
                .collect::<Vec<_>>()
                .join("\n")
        ))
    }
}

#[macro_export]
macro_rules! assert_errors {
    ($a:expr, $b:expr) => {
        match crate::subgraph::subgraph_utils::check_errors(&$a, &$b) {
            Ok(()) => {
                // Success
            }
            Err(e) => {
                panic!("{e}")
            }
        }
    };
}
