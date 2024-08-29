use nom::character::complete::multispace0;
use nom::IResult;
use serde_json_bytes::Value as JSON;

use super::location::Parsed;

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
pub fn spaces_or_comments(input: &str) -> IResult<&str, Parsed<&str>> {
    let mut suffix = input;
    loop {
        (suffix, _) = multispace0(suffix)?;
        let mut chars = suffix.chars();
        if let Some('#') = chars.next() {
            for c in chars.by_ref() {
                if c == '\n' {
                    break;
                }
            }
            suffix = chars.as_str();
        } else {
            return Ok((
                suffix,
                Parsed::new(&input[0..input.len() - suffix.len()], None),
            ));
        }
    }
}

pub fn json_type_name(v: &JSON) -> &str {
    match v {
        JSON::Array(_) => "array",
        JSON::Object(_) => "object",
        JSON::String(_) => "string",
        JSON::Number(_) => "number",
        JSON::Bool(_) => "boolean",
        JSON::Null => "null",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_spaces_or_comments() {
        fn check(input: &str, (exp_remainder, exp_spaces): (&str, &str)) {
            match spaces_or_comments(input) {
                Ok((remainder, parsed)) => {
                    assert_eq!(remainder, exp_remainder);
                    assert_eq!(*parsed.node(), exp_spaces);
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
