use apollo_compiler::collections::IndexSet;
use nom::Slice;
use nom::character::complete::multispace0;
use serde_json_bytes::Map as JSONMap;
use serde_json_bytes::Value as JSON;

use super::ParseResult;
use super::location::Span;
use super::location::WithRange;

// This macro is handy for tests, but it absolutely should never be used with
// dynamic input at runtime, since it panics if the selection string fails to
// parse for any reason.
#[cfg(test)]
#[macro_export]
macro_rules! selection {
    ($input:expr) => {
        if let Ok(parsed) = $crate::sources::connect::json_selection::JSONSelection::parse($input) {
            parsed
        } else {
            panic!("invalid selection: {:?}", $input);
        }
    };
}

// Consumes any amount of whitespace and/or comments starting with # until the
// end of the line.
pub(crate) fn spaces_or_comments(input: Span) -> ParseResult<WithRange<&str>> {
    let mut suffix = input;
    loop {
        let mut made_progress = false;
        let suffix_and_spaces = multispace0(suffix)?;
        suffix = suffix_and_spaces.0;
        if !suffix_and_spaces.1.fragment().is_empty() {
            made_progress = true;
        }
        let suffix_len = suffix.fragment().len();
        if suffix.fragment().starts_with('#') {
            if let Some(newline) = suffix.fragment().find('\n') {
                suffix = suffix.slice(newline + 1..);
            } else {
                suffix = suffix.slice(suffix_len..);
            }
            made_progress = true;
        }
        if !made_progress {
            let end_of_slice = input.fragment().len() - suffix_len;
            let start = input.location_offset();
            let end = suffix.location_offset();
            return Ok((
                suffix,
                WithRange::new(
                    input.slice(0..end_of_slice).fragment(),
                    // The location of the parsed spaces and comments
                    Some(start..end),
                ),
            ));
        }
    }
}

#[allow(unused)]
pub(crate) fn span_is_all_spaces_or_comments(input: Span) -> bool {
    match spaces_or_comments(input) {
        Ok((remainder, _)) => remainder.fragment().is_empty(),
        _ => false,
    }
}

pub(crate) const fn json_type_name(v: &JSON) -> &str {
    match v {
        JSON::Array(_) => "array",
        JSON::Object(_) => "object",
        JSON::String(_) => "string",
        JSON::Number(_) => "number",
        JSON::Bool(_) => "boolean",
        JSON::Null => "null",
    }
}

pub(crate) fn vec_push<T>(mut vec: Vec<T>, item: T) -> Vec<T> {
    vec.push(item);
    vec
}

pub(crate) fn json_merge(a: Option<&JSON>, b: Option<&JSON>) -> (Option<JSON>, Vec<String>) {
    match (a, b) {
        (Some(JSON::Object(a)), Some(JSON::Object(b))) => {
            let mut merged = JSONMap::new();
            let mut errors = Vec::new();

            for key in IndexSet::from_iter(a.keys().chain(b.keys())) {
                let (child_opt, child_errors) = json_merge(a.get(key), b.get(key));
                if let Some(child) = child_opt {
                    merged.insert(key.clone(), child);
                }
                errors.extend(child_errors);
            }

            (Some(JSON::Object(merged)), errors)
        }

        (Some(JSON::Array(a)), Some(JSON::Array(b))) => {
            let max_len = a.len().max(b.len());
            let mut merged = Vec::with_capacity(max_len);
            let mut errors = Vec::new();

            for i in 0..max_len {
                let (child_opt, child_errors) = json_merge(a.get(i), b.get(i));
                if let Some(child) = child_opt {
                    merged.push(child);
                }
                errors.extend(child_errors);
            }

            (Some(JSON::Array(merged)), errors)
        }

        (Some(JSON::Null), _) => (Some(JSON::Null), Vec::new()),
        (_, Some(JSON::Null)) => (Some(JSON::Null), Vec::new()),

        (Some(a), Some(b)) => {
            if a == b {
                (Some(a.clone()), Vec::new())
            } else {
                let json_type_of_a = json_type_name(a);
                let json_type_of_b = json_type_name(b);
                (
                    Some(b.clone()),
                    if json_type_of_a == json_type_of_b {
                        Vec::new()
                    } else {
                        vec![format!(
                            "Lossy merge replacing {} with {}",
                            json_type_of_a, json_type_of_b
                        )]
                    },
                )
            }
        }

        (None, Some(b)) => (Some(b.clone()), Vec::new()),
        (Some(a), None) => (Some(a.clone()), Vec::new()),
        (None, None) => (None, Vec::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::connect::json_selection::location::new_span;

    #[test]
    fn test_spaces_or_comments() {
        fn check(input: &str, (exp_remainder, exp_spaces): (&str, &str)) {
            match spaces_or_comments(new_span(input)) {
                Ok((remainder, parsed)) => {
                    assert_eq!(*remainder.fragment(), exp_remainder);
                    assert_eq!(*parsed.as_ref(), exp_spaces);
                }
                Err(e) => panic!("error: {:?}", e),
            }
        }

        check("", ("", ""));
        check(" ", ("", " "));
        check("  ", ("", "  "));

        check("#", ("", "#"));
        check("# ", ("", "# "));
        check(" # ", ("", " # "));
        check(" #", ("", " #"));

        check("#\n", ("", "#\n"));
        check("# \n", ("", "# \n"));
        check(" # \n", ("", " # \n"));
        check(" #\n", ("", " #\n"));
        check(" # \n ", ("", " # \n "));

        check("hello", ("hello", ""));
        check(" hello", ("hello", " "));
        check("hello ", ("hello ", ""));
        check("hello#", ("hello#", ""));
        check("hello #", ("hello #", ""));
        check("hello # ", ("hello # ", ""));
        check("   hello # ", ("hello # ", "   "));
        check("  hello # world ", ("hello # world ", "  "));

        check("#comment", ("", "#comment"));
        check(" #comment", ("", " #comment"));
        check("#comment ", ("", "#comment "));
        check("#comment#", ("", "#comment#"));
        check("#comment #", ("", "#comment #"));
        check("#comment # ", ("", "#comment # "));
        check("  #comment # world ", ("", "  #comment # world "));
        check("  # comment # world ", ("", "  # comment # world "));

        check(
            "  # comment\nnot a comment",
            ("not a comment", "  # comment\n"),
        );
        check(
            "  # comment\nnot a comment\n",
            ("not a comment\n", "  # comment\n"),
        );
        check(
            "not a comment\n  # comment\nasdf",
            ("not a comment\n  # comment\nasdf", ""),
        );

        #[rustfmt::skip]
        check("
            # This is a comment
            # And so is this
            not a comment
        ", ("not a comment
        ", "
            # This is a comment
            # And so is this
            "));

        #[rustfmt::skip]
        check("
            # This is a comment
            not a comment
            # Another comment
        ", ("not a comment
            # Another comment
        ", "
            # This is a comment
            "));

        #[rustfmt::skip]
        check("
            not a comment
            # This is a comment
            # Another comment
        ", ("not a comment
            # This is a comment
            # Another comment
        ", "
            "));
    }
}
