//! Pretty printing utility methods
//!
//! Working with raw JSONSelections when doing snapshot testing is difficult to
//! read and makes the snapshots themselves quite large. This module adds a new
//! pretty printing trait which is then implemented on the various sub types
//! of the JSONSelection tree.

use crate::sources::connect::json_selection::JSONSelection;
use crate::sources::connect::json_selection::NamedSelection;
use crate::sources::connect::json_selection::PathSelection;
use crate::sources::connect::json_selection::StarSelection;
use crate::sources::connect::json_selection::SubSelection;

/// Pretty print trait
///
/// This trait marks a type as supporting pretty printing itself outside of a
/// Display implementation, which might be more useful for snapshots.
pub trait PrettyPrintable {
    /// Pretty print the struct
    fn pretty_print(&self) -> String {
        self.pretty_print_with_indentation(true, 0)
    }

    /// Pretty print the struct, with indentation
    ///
    /// Each indentation level is marked with 2 spaces, with `inline` signifying
    /// that the first line should be not indented.
    fn pretty_print_with_indentation(&self, inline: bool, indentation: usize) -> String;
}

/// Helper method to generate indentation
fn indent_chars(indent: usize) -> String {
    "  ".repeat(indent)
}

impl PrettyPrintable for JSONSelection {
    fn pretty_print_with_indentation(&self, inline: bool, indentation: usize) -> String {
        let mut result = String::new();

        match self {
            JSONSelection::Named(named) => {
                let named = named.pretty_print_with_indentation(inline, indentation);
                result.push_str(named.as_str());
            }
            JSONSelection::Path(path) => {
                let path = path.pretty_print_with_indentation(inline, indentation);
                result.push_str(path.as_str());
            }
        };

        result
    }
}

impl PrettyPrintable for SubSelection {
    fn pretty_print_with_indentation(&self, inline: bool, indentation: usize) -> String {
        let mut result = String::new();
        let indent = indent_chars(indentation);

        if !inline {
            result.push_str(indent.as_str());
        }

        result.push_str("{\n");

        for selection in &self.selections {
            let selection = selection.pretty_print_with_indentation(false, indentation + 1);
            result.push_str(selection.as_str());
            result.push('\n');
        }

        if let Some(star) = self.star.as_ref() {
            let star = star.pretty_print_with_indentation(false, indentation + 1);
            result.push_str(star.as_str());
            result.push('\n');
        }

        result.push_str(indent.as_str());
        result.push('}');

        result
    }
}

impl PrettyPrintable for PathSelection {
    fn pretty_print_with_indentation(&self, inline: bool, indentation: usize) -> String {
        let mut result = String::new();

        if !inline {
            result.push_str(indent_chars(indentation).as_str());
        }

        match self {
            PathSelection::Var(var, path) => {
                let rest = path.pretty_print_with_indentation(true, indentation);
                result.push_str(var.as_str());
                result.push_str(rest.as_str());
            }
            PathSelection::Key(key, path) => {
                let rest = path.pretty_print_with_indentation(true, indentation);
                result.push_str(key.dotted().as_str());
                result.push_str(rest.as_str());
            }
            PathSelection::Selection(sub) => {
                let sub = sub.pretty_print_with_indentation(true, indentation);
                result.push(' ');
                result.push_str(sub.as_str());
            }
            PathSelection::Empty => {}
        }

        result
    }
}

impl PrettyPrintable for NamedSelection {
    fn pretty_print_with_indentation(&self, inline: bool, indentation: usize) -> String {
        let mut result = String::new();

        if !inline {
            result.push_str(indent_chars(indentation).as_str());
        }

        match self {
            NamedSelection::Field(alias, field_name, sub) => {
                if let Some(alias) = alias {
                    result.push_str(alias.name.as_str());
                    result.push_str(": ");
                }

                result.push_str(field_name.as_str());

                if let Some(sub) = sub {
                    let sub = sub.pretty_print_with_indentation(true, indentation);
                    result.push(' ');
                    result.push_str(sub.as_str());
                }
            }
            NamedSelection::Quoted(alias, literal, sub) => {
                result.push_str(alias.name.as_str());
                result.push_str(": ");

                let safely_quoted =
                    serde_json_bytes::Value::String(literal.clone().into()).to_string();
                result.push_str(safely_quoted.as_str());

                if let Some(sub) = sub {
                    let sub = sub.pretty_print_with_indentation(true, indentation);
                    result.push(' ');
                    result.push_str(sub.as_str());
                }
            }
            NamedSelection::Path(alias, path) => {
                result.push_str(alias.name.as_str());
                result.push_str(": ");

                let path = path.pretty_print_with_indentation(true, indentation);
                result.push_str(path.trim_start());
            }
            NamedSelection::Group(alias, sub) => {
                result.push_str(alias.name.as_str());
                result.push_str(": ");

                let sub = sub.pretty_print_with_indentation(true, indentation);
                result.push_str(sub.as_str());
            }
        };

        result
    }
}

impl PrettyPrintable for StarSelection {
    fn pretty_print_with_indentation(&self, inline: bool, indentation: usize) -> String {
        let mut result = String::new();

        if !inline {
            result.push_str(indent_chars(indentation).as_str());
        }

        if let Some(alias) = self.0.as_ref() {
            result.push_str(alias.name.as_str());
            result.push_str(": ");
        }

        result.push('*');

        if let Some(sub) = self.1.as_ref() {
            let sub = sub.pretty_print_with_indentation(true, indentation);
            result.push(' ');
            result.push_str(sub.as_str());
        }

        result
    }
}

#[cfg(test)]
mod tests {
    use crate::sources::connect::json_selection::pretty::indent_chars;
    use crate::sources::connect::json_selection::NamedSelection;
    use crate::sources::connect::json_selection::PrettyPrintable;
    use crate::sources::connect::json_selection::StarSelection;
    use crate::sources::connect::PathSelection;
    use crate::sources::connect::SubSelection;

    // Test all valid pretty print permutations
    fn test_permutations(selection: impl PrettyPrintable, expected: &str) {
        let indentation = 4;
        let expected_indented = expected
            .lines()
            .map(|line| format!("{}{line}", indent_chars(indentation)))
            .collect::<Vec<_>>()
            .join("\n");

        let prettified = selection.pretty_print();
        assert_eq!(
            prettified, expected,
            "pretty printing did not match: {prettified} != {expected}"
        );

        let prettified_inline = selection.pretty_print_with_indentation(true, indentation);
        assert_eq!(
            prettified_inline,
            expected_indented.trim_start(),
            "pretty printing inline did not match: {prettified_inline} != {}",
            expected_indented.trim_start()
        );

        let prettified_indented = selection.pretty_print_with_indentation(false, indentation);
        assert_eq!(
            prettified_indented, expected_indented,
            "pretty printing indented did not match: {prettified_indented} != {expected_indented}"
        );
    }

    #[test]
    fn it_prints_a_star_selection() {
        let (unmatched, star_selection) = StarSelection::parse("rest: *").unwrap();
        assert!(unmatched.is_empty());

        test_permutations(star_selection, "rest: *");
    }

    #[test]
    fn it_prints_a_star_selection_with_subselection() {
        let (unmatched, star_selection) = StarSelection::parse("rest: * { a b }").unwrap();
        assert!(unmatched.is_empty());

        test_permutations(star_selection, "rest: * {\n  a\n  b\n}");
    }

    #[test]
    fn it_prints_a_named_selection() {
        let selections = [
            // Field
            "cool",
            "cool: beans",
            "cool: beans {\n  whoa\n}",
            // Path
            "cool: .one.two.three",
            // Quoted
            r#"cool: "b e a n s""#,
            "cool: \"b e a n s\" {\n  a\n  b\n}",
            // Group
            "cool: {\n  a\n  b\n}",
        ];
        for selection in selections {
            let (unmatched, named_selection) = NamedSelection::parse(selection).unwrap();
            assert!(
                unmatched.is_empty(),
                "static named selection was not fully parsed: '{selection}' ({named_selection:?}) had unmatched '{unmatched}'"
            );

            test_permutations(named_selection, selection);
        }
    }

    #[test]
    fn it_prints_a_path_selection() {
        let paths = [
            // Var
            "$.one.two.three",
            "$this.a.b",
            "$id.first {\n  username\n}",
            // Key
            ".first",
            ".a.b.c.d.e",
            ".one.two.three {\n  a\n  b\n}",
        ];
        for path in paths {
            let (unmatched, path_selection) = PathSelection::parse(path).unwrap();
            assert!(
                unmatched.is_empty(),
                "static path was not fully parsed: '{path}' ({path_selection:?}) had unmatched '{unmatched}'"
            );

            test_permutations(path_selection, path);
        }
    }

    #[test]
    fn it_prints_a_sub_selection() {
        let sub = "{\n  a\n  b\n}";
        let (unmatched, sub_selection) = SubSelection::parse(sub).unwrap();
        assert!(
            unmatched.is_empty(),
            "static path was not fully parsed: '{sub}' ({sub_selection:?}) had unmatched '{unmatched}'"
        );

        test_permutations(sub_selection, sub);
    }

    #[test]
    fn it_prints_a_nested_sub_selection() {
        let sub = "{
          a {
            b {
              c
            }
          }
        }";
        let sub_indented = "{\n  a {\n    b {\n      c\n    }\n  }\n}";
        let sub_super_indented = "        {\n          a {\n            b {\n              c\n            }\n          }\n        }";

        let (unmatched, sub_selection) = SubSelection::parse(sub).unwrap();
        assert!(
            unmatched.is_empty(),
            "static nested sub was not fully parsed: '{sub}' ({sub_selection:?}) had unmatched '{unmatched}'"
        );

        let pretty = sub_selection.pretty_print();
        assert_eq!(
            pretty, sub_indented,
            "nested sub pretty printing did not match: {pretty} != {sub_indented}"
        );

        let pretty = sub_selection.pretty_print_with_indentation(true, 4);
        assert_eq!(
            pretty,
            sub_super_indented.trim_start(),
            "nested inline sub pretty printing did not match: {pretty} != {}",
            sub_super_indented.trim_start()
        );

        let pretty = sub_selection.pretty_print_with_indentation(false, 4);
        assert_eq!(
            pretty, sub_super_indented,
            "nested inline sub pretty printing did not match: {pretty} != {sub_super_indented}",
        );
    }
}
