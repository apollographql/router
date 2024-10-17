use nom::character::complete::multispace0;
use nom::Slice;
use serde_json_bytes::Value as JSON;

use super::location::Span;
use super::location::WithRange;
use super::ParseResult;

// This macro is handy for tests, but it absolutely should never be used with
// dynamic input at runtime, since it panics if the selection string fails to
// parse for any reason.
#[cfg(test)]
#[macro_export]
macro_rules! selection {
    ($input:expr) => {
        if let Ok((remainder, parsed)) =
            $crate::sources::connect::json_selection::JSONSelection::parse($input)
        {
            assert_eq!(remainder, "");
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

pub(crate) fn json_type_name(v: &JSON) -> &str {
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
