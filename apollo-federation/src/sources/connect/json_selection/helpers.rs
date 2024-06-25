use nom::character::complete::multispace0;
use nom::IResult;
use serde_json_bytes::Value as JSON;

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
pub fn spaces_or_comments(input: &str) -> IResult<&str, &str> {
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
            return Ok((suffix, &input[0..input.len() - suffix.len()]));
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
        assert_eq!(spaces_or_comments(""), Ok(("", "")));
        assert_eq!(spaces_or_comments(" "), Ok(("", " ")));
        assert_eq!(spaces_or_comments("  "), Ok(("", "  ")));

        assert_eq!(spaces_or_comments("#"), Ok(("", "#")));
        assert_eq!(spaces_or_comments("# "), Ok(("", "# ")));
        assert_eq!(spaces_or_comments(" # "), Ok(("", " # ")));
        assert_eq!(spaces_or_comments(" #"), Ok(("", " #")));

        assert_eq!(spaces_or_comments("#\n"), Ok(("", "#\n")));
        assert_eq!(spaces_or_comments("# \n"), Ok(("", "# \n")));
        assert_eq!(spaces_or_comments(" # \n"), Ok(("", " # \n")));
        assert_eq!(spaces_or_comments(" #\n"), Ok(("", " #\n")));
        assert_eq!(spaces_or_comments(" # \n "), Ok(("", " # \n ")));

        assert_eq!(spaces_or_comments("hello"), Ok(("hello", "")));
        assert_eq!(spaces_or_comments(" hello"), Ok(("hello", " ")));
        assert_eq!(spaces_or_comments("hello "), Ok(("hello ", "")));
        assert_eq!(spaces_or_comments("hello#"), Ok(("hello#", "")));
        assert_eq!(spaces_or_comments("hello #"), Ok(("hello #", "")));
        assert_eq!(spaces_or_comments("hello # "), Ok(("hello # ", "")));
        assert_eq!(spaces_or_comments("   hello # "), Ok(("hello # ", "   ")));
        assert_eq!(
            spaces_or_comments("  hello # world "),
            Ok(("hello # world ", "  "))
        );

        assert_eq!(spaces_or_comments("#comment"), Ok(("", "#comment")));
        assert_eq!(spaces_or_comments(" #comment"), Ok(("", " #comment")));
        assert_eq!(spaces_or_comments("#comment "), Ok(("", "#comment ")));
        assert_eq!(spaces_or_comments("#comment#"), Ok(("", "#comment#")));
        assert_eq!(spaces_or_comments("#comment #"), Ok(("", "#comment #")));
        assert_eq!(spaces_or_comments("#comment # "), Ok(("", "#comment # ")));
        assert_eq!(
            spaces_or_comments("  #comment # world "),
            Ok(("", "  #comment # world "))
        );
        assert_eq!(
            spaces_or_comments("  # comment # world "),
            Ok(("", "  # comment # world "))
        );

        assert_eq!(
            spaces_or_comments("  # comment\nnot a comment"),
            Ok(("not a comment", "  # comment\n"))
        );
        assert_eq!(
            spaces_or_comments("  # comment\nnot a comment\n"),
            Ok(("not a comment\n", "  # comment\n"))
        );
        assert_eq!(
            spaces_or_comments("not a comment\n  # comment\nasdf"),
            Ok(("not a comment\n  # comment\nasdf", ""))
        );

        #[rustfmt::skip]
        assert_eq!(spaces_or_comments("
            # This is a comment
            # And so is this
            not a comment
        "),
        Ok(("not a comment
        ", "
            # This is a comment
            # And so is this
            ")));

        #[rustfmt::skip]
        assert_eq!(spaces_or_comments("
            # This is a comment
            not a comment
            # Another comment
        "),
        Ok(("not a comment
            # Another comment
        ", "
            # This is a comment
            ")));

        #[rustfmt::skip]
        assert_eq!(spaces_or_comments("
            not a comment
            # This is a comment
            # Another comment
        "),
        Ok(("not a comment
            # This is a comment
            # Another comment
        ", "
            ")));
    }
}
