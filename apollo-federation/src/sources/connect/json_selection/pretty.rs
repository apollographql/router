//! Pretty printing utility methods
//!
//! Working with raw JSONSelections when doing snapshot testing is difficult to
//! read and makes the snapshots themselves quite large. This module adds a new
//! pretty printing trait which is then implemented on the various sub types
//! of the JSONSelection tree.

use itertools::Itertools;

use super::lit_expr::LitExpr;
use crate::sources::connect::json_selection::JSONSelection;
use crate::sources::connect::json_selection::MethodArgs;
use crate::sources::connect::json_selection::NamedSelection;
use crate::sources::connect::json_selection::PathList;
use crate::sources::connect::json_selection::PathSelection;
use crate::sources::connect::json_selection::StarSelection;
use crate::sources::connect::json_selection::SubSelection;

impl std::fmt::Display for JSONSelection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.pretty_print())
    }
}

/// Pretty print trait
///
/// This trait marks a type as supporting pretty printing itself outside of a
/// Display implementation, which might be more useful for snapshots.
pub(crate) trait PrettyPrintable {
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
        match self {
            JSONSelection::Named(named) => named.print_subselections(indentation),
            JSONSelection::Path(path) => path.pretty_print_with_indentation(inline, indentation),
        }
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

        result.push_str(&self.print_subselections(indentation + 1));

        result.push('\n');
        result.push_str(indent.as_str());
        result.push('}');

        result
    }
}

impl SubSelection {
    /// Prints all of the selections in a subselection
    fn print_subselections(&self, indentation: usize) -> String {
        let selections = self
            .selections
            .iter()
            .map(|s| s.pretty_print_with_indentation(false, indentation));

        let star = self
            .star
            .as_ref()
            .map(|s| s.pretty_print_with_indentation(false, indentation));

        selections.chain(star).join("\n")
    }
}

impl PrettyPrintable for PathSelection {
    fn pretty_print_with_indentation(&self, inline: bool, indentation: usize) -> String {
        let inner = self.path.pretty_print_with_indentation(inline, indentation);
        // Because we can't tell where PathList::Key elements appear in the path
        // once we're inside PathList::pretty_print_with_indentation, we print
        // all PathList::Key elements with a leading '.' character, but we
        // remove the initial '.' if the path has more than one element, because
        // then the leading '.' is not necessary to disambiguate the key from a
        // field. To complicate matters further, inner may begin with spaces due
        // to indentation.
        let leading_space_count = inner.chars().take_while(|c| *c == ' ').count();
        let suffix = inner[leading_space_count..].to_string();
        if suffix.starts_with('.') && !self.path.is_single_key() {
            // Strip the '.' but keep any leading spaces.
            format!("{}{}", " ".repeat(leading_space_count), &suffix[1..])
        } else {
            inner
        }
    }
}

impl PrettyPrintable for PathList {
    fn pretty_print_with_indentation(&self, inline: bool, indentation: usize) -> String {
        let mut result = String::new();

        if !inline {
            result.push_str(indent_chars(indentation).as_str());
        }

        match self {
            Self::Var(var, tail) => {
                let rest = tail.pretty_print_with_indentation(true, indentation);
                result.push_str(var.as_str());
                result.push_str(rest.as_str());
            }
            Self::Key(key, tail) => {
                let rest = tail.pretty_print_with_indentation(true, indentation);
                result.push_str(key.dotted().as_str());
                result.push_str(rest.as_str());
            }
            Self::Expr(expr, tail) => {
                let rest = tail.pretty_print_with_indentation(true, indentation);
                result.push_str("$(");
                result.push_str(
                    expr.pretty_print_with_indentation(true, indentation)
                        .as_str(),
                );
                result.push(')');
                result.push_str(rest.as_str());
            }
            Self::Method(method, args, tail) => {
                result.push_str("->");
                result.push_str(method.as_str());
                if let Some(args) = args {
                    result.push_str(
                        args.pretty_print_with_indentation(true, indentation)
                            .as_str(),
                    );
                }
                result.push_str(
                    tail.pretty_print_with_indentation(true, indentation)
                        .as_str(),
                );
            }
            Self::Selection(sub) => {
                let sub = sub.pretty_print_with_indentation(true, indentation);
                result.push(' ');
                result.push_str(sub.as_str());
            }
            Self::Empty => {}
        }

        result
    }
}

impl PrettyPrintable for MethodArgs {
    fn pretty_print_with_indentation(&self, inline: bool, indentation: usize) -> String {
        let mut result = String::new();

        if !inline {
            result.push_str(indent_chars(indentation).as_str());
        }

        result.push('(');

        // TODO Break long argument lists across multiple lines, with indentation?
        for (i, arg) in self.args.iter().enumerate() {
            if i > 0 {
                result.push_str(", ");
            }
            result.push_str(
                arg.pretty_print_with_indentation(true, indentation)
                    .as_str(),
            );
        }

        result.push(')');

        result
    }
}

impl PrettyPrintable for LitExpr {
    fn pretty_print_with_indentation(&self, inline: bool, indentation: usize) -> String {
        let mut result = String::new();
        if !inline {
            result.push_str(indent_chars(indentation).as_str());
        }

        match self {
            Self::String(s) => {
                let safely_quoted = serde_json_bytes::Value::String(s.clone().into()).to_string();
                result.push_str(safely_quoted.as_str());
            }
            Self::Number(n) => result.push_str(n.to_string().as_str()),
            Self::Bool(b) => result.push_str(b.to_string().as_str()),
            Self::Null => result.push_str("null"),
            Self::Object(map) => {
                result.push('{');
                let mut is_first = true;
                for (key, value) in map {
                    if is_first {
                        is_first = false;
                    } else {
                        result.push_str(", ");
                    }
                    let key = serde_json_bytes::Value::String(key.as_str().into()).to_string();
                    result.push_str(key.as_str());
                    result.push_str(": ");
                    result.push_str(
                        value
                            .pretty_print_with_indentation(true, indentation)
                            .as_str(),
                    );
                }
                result.push('}');
            }
            Self::Array(vec) => {
                result.push('[');
                let mut is_first = true;
                for value in vec {
                    if is_first {
                        is_first = false;
                    } else {
                        result.push_str(", ");
                    }
                    result.push_str(
                        value
                            .pretty_print_with_indentation(true, indentation)
                            .as_str(),
                    );
                }
                result.push(']');
            }
            Self::Path(path) => {
                let path = path.pretty_print_with_indentation(inline, indentation);
                result.push_str(path.as_str());
            }
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
            Self::Field(alias, field_key, sub) => {
                if let Some(alias) = alias {
                    result.push_str(alias.name.as_str());
                    result.push_str(": ");
                }

                if field_key.is_quoted() {
                    let safely_quoted =
                        serde_json_bytes::Value::String(field_key.as_str().into()).to_string();
                    result.push_str(safely_quoted.as_str());
                } else {
                    result.push_str(field_key.as_str());
                }

                if let Some(sub) = sub {
                    let sub = sub.pretty_print_with_indentation(true, indentation);
                    result.push(' ');
                    result.push_str(sub.as_str());
                }
            }
            Self::Path(alias_opt, path) => {
                if let Some(alias) = alias_opt {
                    result.push_str(alias.name.as_str());
                    result.push_str(": ");
                }
                let path = path.pretty_print_with_indentation(true, indentation);
                result.push_str(path.trim_start());
            }
            Self::Group(alias, sub) => {
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

        if let Some(alias) = &self.alias {
            result.push_str(alias.name.as_str());
            result.push_str(": ");
        }

        result.push('*');

        if let Some(sub) = &self.selection {
            let sub = sub.pretty_print_with_indentation(true, indentation);
            result.push(' ');
            result.push_str(sub.as_str());
        }

        result
    }
}

#[cfg(test)]
mod tests {
    use super::super::location::Span;
    use crate::sources::connect::json_selection::pretty::indent_chars;
    use crate::sources::connect::json_selection::NamedSelection;
    use crate::sources::connect::json_selection::PrettyPrintable;
    use crate::sources::connect::json_selection::StarSelection;
    use crate::sources::connect::JSONSelection;
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
            prettified_inline.trim_start(),
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
        let (unmatched, star_selection) = StarSelection::parse(Span::new("rest: *")).unwrap();
        assert!(unmatched.is_empty());

        test_permutations(star_selection, "rest: *");
    }

    #[test]
    fn it_prints_a_star_selection_with_subselection() {
        let (unmatched, star_selection) =
            StarSelection::parse(Span::new("rest: * { a b }")).unwrap();
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
            "cool: one.two.three",
            // Quoted
            r#"cool: "b e a n s""#,
            "cool: \"b e a n s\" {\n  a\n  b\n}",
            // Group
            "cool: {\n  a\n  b\n}",
        ];
        for selection in selections {
            let (unmatched, named_selection) = NamedSelection::parse(Span::new(selection)).unwrap();
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
            "$this.id.first {\n  username\n}",
            // Key
            "$.first",
            "a.b.c.d.e",
            "one.two.three {\n  a\n  b\n}",
            "$.single {\n  x\n}",
            "results->slice($(-1)->mul($args.suffixLength))",
            "$(1234)->add($(5678)->mul(2))",
            "$(true)->and($(false)->not)",
            "$(12345678987654321)->div(111111111)->eq(111111111)",
            "$(\"Product\")->slice(0, $(4)->mul(-1))->eq(\"Pro\")",
            "$($args.unnecessary.parens)->eq(42)",
        ];
        for path in paths {
            let (unmatched, path_selection) = PathSelection::parse(Span::new(path)).unwrap();
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
        let (unmatched, sub_selection) = SubSelection::parse(Span::new(sub)).unwrap();
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

        let (unmatched, sub_selection) = SubSelection::parse(Span::new(sub)).unwrap();

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

    #[test]
    fn it_prints_root_selection() {
        let (unmatched, root_selection) = JSONSelection::parse("id name").unwrap();
        assert!(unmatched.is_empty());

        test_permutations(root_selection, "id\nname");
    }
}
