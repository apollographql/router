use std::fmt::Display;

use nom::branch::alt;
use nom::character::complete::char;
use nom::character::complete::one_of;
use nom::combinator::all_consuming;
use nom::combinator::map;
use nom::combinator::opt;
use nom::combinator::recognize;
use nom::multi::many0;
use nom::sequence::delimited;
use nom::sequence::pair;
use nom::sequence::preceded;
use nom::sequence::tuple;
use nom::IResult;
use serde::Serialize;
use serde_json_bytes::Value as JSON;

use super::helpers::spaces_or_comments;

// JSONSelection     ::= NakedSubSelection | PathSelection
// NakedSubSelection ::= NamedSelection* StarSelection?

#[derive(Debug, PartialEq, Clone, Serialize)]
pub enum JSONSelection {
    // Although we reuse the SubSelection type for the JSONSelection::Named
    // case, we parse it as a sequence of NamedSelection items without the
    // {...} curly braces that SubSelection::parse expects.
    Named(SubSelection),
    Path(PathSelection),
}

impl JSONSelection {
    pub fn empty() -> Self {
        JSONSelection::Named(SubSelection {
            selections: vec![],
            star: None,
        })
    }

    pub fn is_empty(&self) -> bool {
        match self {
            JSONSelection::Named(subselect) => {
                subselect.selections.is_empty() && subselect.star.is_none()
            }
            JSONSelection::Path(path) => path == &PathSelection::Empty,
        }
    }

    pub fn parse(input: &str) -> IResult<&str, Self> {
        alt((
            all_consuming(map(
                tuple((
                    many0(NamedSelection::parse),
                    // When a * selection is used, it must be the last selection
                    // in the sequence, since it is not a NamedSelection.
                    opt(StarSelection::parse),
                    // In case there were no named selections and no * selection, we
                    // still want to consume any space before the end of the input.
                    spaces_or_comments,
                )),
                |(selections, star, _)| Self::Named(SubSelection { selections, star }),
            )),
            all_consuming(map(PathSelection::parse, Self::Path)),
        ))(input)
    }

    pub(crate) fn next_subselection(&self) -> Option<&SubSelection> {
        match self {
            JSONSelection::Named(subselect) => Some(subselect),
            JSONSelection::Path(path) => path.next_subselection(),
        }
    }

    pub(crate) fn next_mut_subselection(&mut self) -> Option<&mut SubSelection> {
        match self {
            JSONSelection::Named(subselect) => Some(subselect),
            JSONSelection::Path(path) => path.next_mut_subselection(),
        }
    }
}

// NamedSelection       ::= NamedPathSelection | NamedFieldSelection | NamedQuotedSelection | NamedGroupSelection
// NamedPathSelection   ::= Alias PathSelection
// NamedFieldSelection  ::= Alias? Identifier SubSelection?
// NamedQuotedSelection ::= Alias StringLiteral SubSelection?
// NamedGroupSelection  ::= Alias SubSelection

#[derive(Debug, PartialEq, Clone, Serialize)]
pub enum NamedSelection {
    Field(Option<Alias>, String, Option<SubSelection>),
    Quoted(Alias, String, Option<SubSelection>),
    Path(Alias, PathSelection),
    Group(Alias, SubSelection),
}

impl NamedSelection {
    pub(crate) fn parse(input: &str) -> IResult<&str, Self> {
        alt((
            // We must try parsing NamedPathSelection before NamedFieldSelection
            // and NamedQuotedSelection because a NamedPathSelection without a
            // leading `.`, such as `alias: some.nested.path` has a prefix that
            // can be parsed as a NamedFieldSelection: `alias: some`. Parsing
            // then fails when it finds the remaining `.nested.path` text. Some
            // parsers would solve this by forbidding `.` in the "lookahead" for
            // Named{Field,Quoted}Selection, but negative lookahead is tricky in
            // nom, so instead we greedily parse NamedPathSelection first.
            Self::parse_path,
            Self::parse_field,
            Self::parse_quoted,
            Self::parse_group,
        ))(input)
    }

    fn parse_field(input: &str) -> IResult<&str, Self> {
        tuple((
            opt(Alias::parse),
            delimited(spaces_or_comments, parse_identifier, spaces_or_comments),
            opt(SubSelection::parse),
        ))(input)
        .map(|(input, (alias, name, selection))| (input, Self::Field(alias, name, selection)))
    }

    fn parse_quoted(input: &str) -> IResult<&str, Self> {
        tuple((
            Alias::parse,
            delimited(spaces_or_comments, parse_string_literal, spaces_or_comments),
            opt(SubSelection::parse),
        ))(input)
        .map(|(input, (alias, name, selection))| (input, Self::Quoted(alias, name, selection)))
    }

    fn parse_path(input: &str) -> IResult<&str, Self> {
        tuple((Alias::parse, PathSelection::parse))(input)
            .map(|(input, (alias, path))| (input, Self::Path(alias, path)))
    }

    fn parse_group(input: &str) -> IResult<&str, Self> {
        tuple((Alias::parse, SubSelection::parse))(input)
            .map(|(input, (alias, group))| (input, Self::Group(alias, group)))
    }

    #[allow(dead_code)]
    pub(crate) fn name(&self) -> &str {
        match self {
            Self::Field(alias, name, _) => {
                if let Some(alias) = alias {
                    alias.name.as_str()
                } else {
                    name.as_str()
                }
            }
            Self::Quoted(alias, _, _) => alias.name.as_str(),
            Self::Path(alias, _) => alias.name.as_str(),
            Self::Group(alias, _) => alias.name.as_str(),
        }
    }

    /// Extracts the property path for a given named selection
    ///
    // TODO: Expand on what this means once I have a better understanding
    pub(crate) fn property_path(&self) -> Vec<Key> {
        match self {
            NamedSelection::Field(_, name, _) => vec![Key::Field(name.to_string())],
            NamedSelection::Quoted(_, _, Some(_)) => todo!(),
            NamedSelection::Quoted(_, name, None) => vec![Key::Quoted(name.to_string())],
            NamedSelection::Path(_, path) => path.collect_paths(),
            NamedSelection::Group(alias, _) => vec![Key::Field(alias.name.to_string())],
        }
    }

    /// Find the next subselection, if present
    pub(crate) fn next_subselection(&self) -> Option<&SubSelection> {
        match self {
            // Paths are complicated because they can have a subselection deeply nested
            NamedSelection::Path(_, path) => path.next_subselection(),

            // The other options have it at the root
            NamedSelection::Field(_, _, Some(sub))
            | NamedSelection::Quoted(_, _, Some(sub))
            | NamedSelection::Group(_, sub) => Some(sub),

            // Every other option does not have a subselection
            _ => None,
        }
    }

    pub(crate) fn next_mut_subselection(&mut self) -> Option<&mut SubSelection> {
        match self {
            // Paths are complicated because they can have a subselection deeply nested
            NamedSelection::Path(_, path) => path.next_mut_subselection(),

            // The other options have it at the root
            NamedSelection::Field(_, _, Some(sub))
            | NamedSelection::Quoted(_, _, Some(sub))
            | NamedSelection::Group(_, sub) => Some(sub),

            // Every other option does not have a subselection
            _ => None,
        }
    }
}

// PathSelection ::= (VarPath | KeyPath) SubSelection?
// VarPath       ::= "$" (NO_SPACE Identifier)? PathStep*
// KeyPath       ::= Key PathStep+
// PathStep      ::= "." Key | "->" Identifier MethodArgs?

#[derive(Debug, PartialEq, Clone, Serialize)]
pub enum PathSelection {
    // We use a recursive structure here instead of a Vec<Key> to make applying
    // the selection to a JSON value easier.
    Var(String, Box<PathSelection>),
    Key(Key, Box<PathSelection>),
    Selection(SubSelection),
    Empty,
}

impl PathSelection {
    pub(crate) fn parse(input: &str) -> IResult<&str, Self> {
        match Self::parse_with_depth(input, 0) {
            Ok((remainder, Self::Empty)) => Err(nom::Err::Error(nom::error::Error::new(
                remainder,
                nom::error::ErrorKind::IsNot,
            ))),
            otherwise => otherwise,
        }
    }

    fn parse_with_depth(input: &str, depth: usize) -> IResult<&str, Self> {
        let (input, _spaces) = spaces_or_comments(input)?;

        // Variable references and key references without a leading . are
        // accepted only at depth 0, or at the beginning of the PathSelection.
        if depth == 0 {
            if let Ok((suffix, opt_var)) = delimited(
                tuple((spaces_or_comments, char('$'))),
                opt(parse_identifier),
                spaces_or_comments,
            )(input)
            {
                let (input, rest) = Self::parse_with_depth(suffix, depth + 1)?;
                // Note the $ prefix is included in the variable name.
                let dollar_var = format!("${}", opt_var.unwrap_or("".to_string()));
                return Ok((input, Self::Var(dollar_var, Box::new(rest))));
            }

            if let Ok((suffix, key)) = Key::parse(input) {
                let (input, rest) = Self::parse_with_depth(suffix, depth + 1)?;
                return match rest {
                    Self::Empty | Self::Selection(_) => Err(nom::Err::Error(
                        nom::error::Error::new(input, nom::error::ErrorKind::IsNot),
                    )),
                    rest => Ok((input, Self::Key(key, Box::new(rest)))),
                };
            }
        }

        // The .key case is applicable at any depth. If it comes first in the
        // path selection, $.key is implied, but the distinction is preserved
        // (using Self::Path rather than Self::Var) for accurate reprintability.
        if let Ok((suffix, key)) = preceded(
            tuple((spaces_or_comments, char('.'), spaces_or_comments)),
            Key::parse,
        )(input)
        {
            // tuple((char('.'), Key::parse))(input) {
            let (input, rest) = Self::parse_with_depth(suffix, depth + 1)?;
            return Ok((input, Self::Key(key, Box::new(rest))));
        }

        if depth == 0 {
            // If the PathSelection does not start with a $var, a key., or a
            // .key, it is not a valid PathSelection.
            return Err(nom::Err::Error(nom::error::Error::new(
                input,
                nom::error::ErrorKind::IsNot,
            )));
        }

        // If the PathSelection has a SubSelection, it must appear at the end of
        // a non-empty path.
        if let Ok((suffix, selection)) = SubSelection::parse(input) {
            return Ok((suffix, Self::Selection(selection)));
        }

        // The Self::Empty enum case is used to indicate the end of a
        // PathSelection that has no SubSelection.
        Ok((input, Self::Empty))
    }

    pub(crate) fn from_slice(properties: &[Key], selection: Option<SubSelection>) -> Self {
        match properties {
            [] => selection.map_or(Self::Empty, Self::Selection),
            [head, tail @ ..] => {
                Self::Key(head.clone(), Box::new(Self::from_slice(tail, selection)))
            }
        }
    }

    /// Collect all nested paths
    ///
    /// This method attempts to collect as many paths as possible, shorting out once
    /// a non path selection is encountered.
    pub(crate) fn collect_paths(&self) -> Vec<Key> {
        let mut results = Vec::new();

        // Collect as many as possible
        let mut current = self;
        while let Self::Key(key, rest) = current {
            results.push(key.clone());

            current = rest;
        }

        results
    }

    /// Find the next subselection, traversing nested chains if needed
    pub(crate) fn next_subselection(&self) -> Option<&SubSelection> {
        match self {
            PathSelection::Var(_, path) => path.next_subselection(),
            PathSelection::Key(_, path) => path.next_subselection(),
            PathSelection::Selection(sub) => Some(sub),
            PathSelection::Empty => None,
        }
    }

    /// Find the next subselection, traversing nested chains if needed. Returns a mutable reference
    pub(crate) fn next_mut_subselection(&mut self) -> Option<&mut SubSelection> {
        match self {
            PathSelection::Var(_, path) => path.next_mut_subselection(),
            PathSelection::Key(_, path) => path.next_mut_subselection(),
            PathSelection::Selection(sub) => Some(sub),
            PathSelection::Empty => None,
        }
    }
}

// SubSelection ::= "{" NakedSubSelection "}"

#[derive(Debug, PartialEq, Clone, Serialize, Default)]
pub struct SubSelection {
    pub(super) selections: Vec<NamedSelection>,
    pub(super) star: Option<StarSelection>,
}

impl SubSelection {
    pub(crate) fn parse(input: &str) -> IResult<&str, Self> {
        tuple((
            spaces_or_comments,
            char('{'),
            many0(NamedSelection::parse),
            // Note that when a * selection is used, it must be the last
            // selection in the SubSelection, since it does not count as a
            // NamedSelection, and is stored as a separate field from the
            // selections vector.
            opt(StarSelection::parse),
            spaces_or_comments,
            char('}'),
            spaces_or_comments,
        ))(input)
        .map(|(input, (_, _, selections, star, _, _, _))| (input, Self { selections, star }))
    }

    pub fn selections_iter(&self) -> impl Iterator<Item = &NamedSelection> {
        self.selections.iter()
    }

    pub fn has_star(&self) -> bool {
        self.star.is_some()
    }

    pub fn set_star(&mut self, star: Option<StarSelection>) {
        self.star = star;
    }

    pub fn append_selection(&mut self, selection: NamedSelection) {
        self.selections.push(selection);
    }

    pub fn last_selection_mut(&mut self) -> Option<&mut NamedSelection> {
        self.selections.last_mut()
    }

    // Since we enforce that new selections may only be appended to
    // self.selections, we can provide an index-based search method that returns
    // an unforgeable NamedSelectionIndex, which can later be used to access the
    // selection using either get_at_index or get_at_index_mut.
    // TODO In the future, this method could make use of an internal lookup
    // table to avoid linear search.
    pub fn index_of_named_selection(&self, name: &str) -> Option<NamedSelectionIndex> {
        self.selections
            .iter()
            .position(|selection| selection.name() == name)
            .map(|pos| NamedSelectionIndex { pos })
    }

    pub fn get_at_index(&self, index: &NamedSelectionIndex) -> &NamedSelection {
        self.selections
            .get(index.pos)
            .expect("NamedSelectionIndex out of bounds")
    }

    pub fn get_at_index_mut(&mut self, index: &NamedSelectionIndex) -> &mut NamedSelection {
        self.selections
            .get_mut(index.pos)
            .expect("NamedSelectionIndex out of bounds")
    }
}

pub struct NamedSelectionIndex {
    // Intentionally private so NamedSelectionIndex cannot be forged.
    pos: usize,
}

// StarSelection ::= Alias? "*" SubSelection?

#[derive(Debug, PartialEq, Clone, Serialize)]
pub struct StarSelection(
    pub(super) Option<Alias>,
    pub(super) Option<Box<SubSelection>>,
);

impl StarSelection {
    pub(crate) fn new(alias: Option<Alias>, sub: Option<SubSelection>) -> Self {
        Self(alias, sub.map(Box::new))
    }

    pub(crate) fn parse(input: &str) -> IResult<&str, Self> {
        tuple((
            // The spaces_or_comments separators are necessary here because
            // Alias::parse and SubSelection::parse only consume surrounding
            // spaces when they match, and they are both optional here.
            opt(Alias::parse),
            spaces_or_comments,
            char('*'),
            spaces_or_comments,
            opt(SubSelection::parse),
        ))(input)
        .map(|(remainder, (alias, _, _, _, selection))| {
            (remainder, Self(alias, selection.map(Box::new)))
        })
    }
}

// Alias ::= Identifier ":"

#[derive(Debug, PartialEq, Clone, Serialize)]
pub struct Alias {
    pub(super) name: String,
}

impl Alias {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
        }
    }

    fn parse(input: &str) -> IResult<&str, Self> {
        tuple((
            spaces_or_comments,
            parse_identifier,
            spaces_or_comments,
            char(':'),
            spaces_or_comments,
        ))(input)
        .map(|(input, (_, name, _, _, _))| (input, Self { name }))
    }

    pub fn name(&self) -> &str {
        self.name.as_str()
    }
}

// Key ::= Identifier | StringLiteral

#[derive(Debug, PartialEq, Eq, Hash, Clone, Serialize)]
pub enum Key {
    Field(String),
    Quoted(String),
    Index(usize),
}

impl Key {
    fn parse(input: &str) -> IResult<&str, Self> {
        alt((
            map(parse_identifier, Self::Field),
            map(parse_string_literal, Self::Quoted),
        ))(input)
    }

    pub fn to_json(&self) -> JSON {
        match self {
            Key::Field(name) => JSON::String(name.clone().into()),
            Key::Quoted(name) => JSON::String(name.clone().into()),
            Key::Index(index) => JSON::Number((*index).into()),
        }
    }

    // This method returns the field/property name as a String, and is
    // appropriate for accessing JSON properties, in contrast to the dotted
    // method below.
    pub fn as_string(&self) -> String {
        match self {
            Key::Field(name) => name.clone(),
            Key::Quoted(name) => name.clone(),
            Key::Index(n) => n.to_string(),
        }
    }

    // This method is used to implement the Display trait for Key, and includes
    // a leading '.' character for string keys, as well as proper quoting for
    // Key::Quoted values. However, these additions make key.dotted() unsafe to
    // use for accessing JSON properties.
    pub fn dotted(&self) -> String {
        match self {
            Key::Field(field) => format!(".{field}"),
            Key::Quoted(field) => {
                // JSON encoding is a reliable way to ensure a string that may
                // contain special characters (such as '"' characters) is
                // properly escaped and double-quoted.
                let quoted = serde_json_bytes::Value::String(field.clone().into()).to_string();
                format!(".{quoted}")
            }
            Key::Index(index) => format!(".{index}"),
        }
    }
}

impl Display for Key {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let dotted = self.dotted();
        write!(f, "{dotted}")
    }
}

// Identifier ::= [a-zA-Z_] NO_SPACE [0-9a-zA-Z_]*

fn parse_identifier(input: &str) -> IResult<&str, String> {
    delimited(
        spaces_or_comments,
        recognize(pair(
            one_of("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ_"),
            many0(one_of(
                "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ_0123456789",
            )),
        )),
        spaces_or_comments,
    )(input)
    .map(|(input, name)| (input, name.to_string()))
}

// StringLiteral ::=
//   | "'" ("\\'" | [^'])* "'"
//   | '"' ('\\"' | [^"])* '"'

fn parse_string_literal(input: &str) -> IResult<&str, String> {
    let input = spaces_or_comments(input).map(|(input, _)| input)?;
    let mut input_char_indices = input.char_indices();

    match input_char_indices.next() {
        Some((0, quote @ '\'')) | Some((0, quote @ '"')) => {
            let mut escape_next = false;
            let mut chars: Vec<char> = vec![];
            let mut remainder: Option<&str> = None;

            for (i, c) in input_char_indices {
                if escape_next {
                    match c {
                        'n' => chars.push('\n'),
                        _ => chars.push(c),
                    }
                    escape_next = false;
                    continue;
                }
                if c == '\\' {
                    escape_next = true;
                    continue;
                }
                if c == quote {
                    remainder = Some(spaces_or_comments(&input[i + 1..])?.0);
                    break;
                }
                chars.push(c);
            }

            if let Some(remainder) = remainder {
                Ok((remainder, chars.iter().collect::<String>()))
            } else {
                Err(nom::Err::Error(nom::error::Error::new(
                    input,
                    nom::error::ErrorKind::Eof,
                )))
            }
        }

        _ => Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::IsNot,
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::selection;

    #[test]
    fn test_identifier() {
        assert_eq!(parse_identifier("hello"), Ok(("", "hello".to_string())),);

        assert_eq!(
            parse_identifier("hello_world"),
            Ok(("", "hello_world".to_string())),
        );

        assert_eq!(
            parse_identifier("hello_world_123"),
            Ok(("", "hello_world_123".to_string())),
        );

        assert_eq!(parse_identifier(" hello "), Ok(("", "hello".to_string())),);
    }

    #[test]
    fn test_string_literal() {
        assert_eq!(
            parse_string_literal("'hello world'"),
            Ok(("", "hello world".to_string())),
        );
        assert_eq!(
            parse_string_literal("\"hello world\""),
            Ok(("", "hello world".to_string())),
        );
        assert_eq!(
            parse_string_literal("'hello \"world\"'"),
            Ok(("", "hello \"world\"".to_string())),
        );
        assert_eq!(
            parse_string_literal("\"hello \\\"world\\\"\""),
            Ok(("", "hello \"world\"".to_string())),
        );
        assert_eq!(
            parse_string_literal("'hello \\'world\\''"),
            Ok(("", "hello 'world'".to_string())),
        );
    }
    #[test]
    fn test_key() {
        assert_eq!(
            Key::parse("hello"),
            Ok(("", Key::Field("hello".to_string()))),
        );

        assert_eq!(
            Key::parse("'hello'"),
            Ok(("", Key::Quoted("hello".to_string()))),
        );
    }

    #[test]
    fn test_alias() {
        assert_eq!(
            Alias::parse("hello:"),
            Ok((
                "",
                Alias {
                    name: "hello".to_string(),
                },
            )),
        );

        assert_eq!(
            Alias::parse("hello :"),
            Ok((
                "",
                Alias {
                    name: "hello".to_string(),
                },
            )),
        );

        assert_eq!(
            Alias::parse("hello : "),
            Ok((
                "",
                Alias {
                    name: "hello".to_string(),
                },
            )),
        );

        assert_eq!(
            Alias::parse("  hello :"),
            Ok((
                "",
                Alias {
                    name: "hello".to_string(),
                },
            )),
        );

        assert_eq!(
            Alias::parse("hello: "),
            Ok((
                "",
                Alias {
                    name: "hello".to_string(),
                },
            )),
        );
    }

    #[test]
    fn test_named_selection() {
        fn assert_result_and_name(input: &str, expected: NamedSelection, name: &str) {
            let actual = NamedSelection::parse(input);
            assert_eq!(actual, Ok(("", expected.clone())));
            assert_eq!(actual.unwrap().1.name(), name);
            assert_eq!(
                selection!(input),
                JSONSelection::Named(SubSelection {
                    selections: vec![expected],
                    star: None,
                }),
            );
        }

        assert_result_and_name(
            "hello",
            NamedSelection::Field(None, "hello".to_string(), None),
            "hello",
        );

        assert_result_and_name(
            "hello { world }",
            NamedSelection::Field(
                None,
                "hello".to_string(),
                Some(SubSelection {
                    selections: vec![NamedSelection::Field(None, "world".to_string(), None)],
                    star: None,
                }),
            ),
            "hello",
        );

        assert_result_and_name(
            "hi: hello",
            NamedSelection::Field(
                Some(Alias {
                    name: "hi".to_string(),
                }),
                "hello".to_string(),
                None,
            ),
            "hi",
        );

        assert_result_and_name(
            "hi: 'hello world'",
            NamedSelection::Quoted(
                Alias {
                    name: "hi".to_string(),
                },
                "hello world".to_string(),
                None,
            ),
            "hi",
        );

        assert_result_and_name(
            "hi: hello { world }",
            NamedSelection::Field(
                Some(Alias {
                    name: "hi".to_string(),
                }),
                "hello".to_string(),
                Some(SubSelection {
                    selections: vec![NamedSelection::Field(None, "world".to_string(), None)],
                    star: None,
                }),
            ),
            "hi",
        );

        assert_result_and_name(
            "hey: hello { world again }",
            NamedSelection::Field(
                Some(Alias {
                    name: "hey".to_string(),
                }),
                "hello".to_string(),
                Some(SubSelection {
                    selections: vec![
                        NamedSelection::Field(None, "world".to_string(), None),
                        NamedSelection::Field(None, "again".to_string(), None),
                    ],
                    star: None,
                }),
            ),
            "hey",
        );

        assert_result_and_name(
            "hey: 'hello world' { again }",
            NamedSelection::Quoted(
                Alias {
                    name: "hey".to_string(),
                },
                "hello world".to_string(),
                Some(SubSelection {
                    selections: vec![NamedSelection::Field(None, "again".to_string(), None)],
                    star: None,
                }),
            ),
            "hey",
        );

        assert_result_and_name(
            "leggo: 'my ego'",
            NamedSelection::Quoted(
                Alias {
                    name: "leggo".to_string(),
                },
                "my ego".to_string(),
                None,
            ),
            "leggo",
        );
    }

    #[test]
    fn test_selection() {
        assert_eq!(
            selection!(""),
            JSONSelection::Named(SubSelection {
                selections: vec![],
                star: None,
            }),
        );

        assert_eq!(
            selection!("   "),
            JSONSelection::Named(SubSelection {
                selections: vec![],
                star: None,
            }),
        );

        assert_eq!(
            selection!("hello"),
            JSONSelection::Named(SubSelection {
                selections: vec![NamedSelection::Field(None, "hello".to_string(), None),],
                star: None,
            }),
        );

        assert_eq!(
            selection!(".hello"),
            JSONSelection::Path(PathSelection::from_slice(
                &[Key::Field("hello".to_string()),],
                None
            )),
        );

        {
            let expected = JSONSelection::Named(SubSelection {
                selections: vec![NamedSelection::Path(
                    Alias {
                        name: "hi".to_string(),
                    },
                    PathSelection::from_slice(
                        &[
                            Key::Field("hello".to_string()),
                            Key::Field("world".to_string()),
                        ],
                        None,
                    ),
                )],
                star: None,
            });

            assert_eq!(selection!("hi: .hello.world"), expected);
            assert_eq!(selection!("hi: .hello .world"), expected);
            assert_eq!(selection!("hi: . hello. world"), expected);
            assert_eq!(selection!("hi: .hello . world"), expected);
            assert_eq!(selection!("hi: hello.world"), expected);
            assert_eq!(selection!("hi: hello. world"), expected);
            assert_eq!(selection!("hi: hello .world"), expected);
            assert_eq!(selection!("hi: hello . world"), expected);
        }

        {
            let expected = JSONSelection::Named(SubSelection {
                selections: vec![
                    NamedSelection::Field(None, "before".to_string(), None),
                    NamedSelection::Path(
                        Alias {
                            name: "hi".to_string(),
                        },
                        PathSelection::from_slice(
                            &[
                                Key::Field("hello".to_string()),
                                Key::Field("world".to_string()),
                            ],
                            None,
                        ),
                    ),
                    NamedSelection::Field(None, "after".to_string(), None),
                ],
                star: None,
            });

            assert_eq!(selection!("before hi: .hello.world after"), expected);
            assert_eq!(selection!("before hi: .hello .world after"), expected);
            assert_eq!(selection!("before hi: .hello. world after"), expected);
            assert_eq!(selection!("before hi: .hello . world after"), expected);
            assert_eq!(selection!("before hi: . hello.world after"), expected);
            assert_eq!(selection!("before hi: . hello .world after"), expected);
            assert_eq!(selection!("before hi: . hello. world after"), expected);
            assert_eq!(selection!("before hi: . hello . world after"), expected);
            assert_eq!(selection!("before hi: hello.world after"), expected);
            assert_eq!(selection!("before hi: hello .world after"), expected);
            assert_eq!(selection!("before hi: hello. world after"), expected);
            assert_eq!(selection!("before hi: hello . world after"), expected);
        }

        {
            let expected = JSONSelection::Named(SubSelection {
                selections: vec![
                    NamedSelection::Field(None, "before".to_string(), None),
                    NamedSelection::Path(
                        Alias {
                            name: "hi".to_string(),
                        },
                        PathSelection::from_slice(
                            &[
                                Key::Field("hello".to_string()),
                                Key::Field("world".to_string()),
                            ],
                            Some(SubSelection {
                                selections: vec![
                                    NamedSelection::Field(None, "nested".to_string(), None),
                                    NamedSelection::Field(None, "names".to_string(), None),
                                ],
                                star: None,
                            }),
                        ),
                    ),
                    NamedSelection::Field(None, "after".to_string(), None),
                ],
                star: None,
            });

            assert_eq!(
                selection!("before hi: .hello.world { nested names } after"),
                expected
            );
            assert_eq!(
                selection!("before hi:.hello.world{nested names}after"),
                expected
            );
            assert_eq!(
                selection!("before hi: hello.world { nested names } after"),
                expected
            );
            assert_eq!(
                selection!("before hi:hello.world{nested names}after"),
                expected
            );
        }

        assert_eq!(
            selection!(
                "
            # Comments are supported because we parse them as whitespace
            topLevelAlias: topLevelField {
                # Non-identifier properties must be aliased as an identifier
                nonIdentifier: 'property name with spaces'

                # This extracts the value located at the given path and applies a
                # selection set to it before renaming the result to pathSelection
                pathSelection: .some.nested.path {
                    still: yet
                    more
                    properties
                }

                # An aliased SubSelection of fields nests the fields together
                # under the given alias
                siblingGroup: { brother sister }
            }"
            ),
            JSONSelection::Named(SubSelection {
                selections: vec![NamedSelection::Field(
                    Some(Alias {
                        name: "topLevelAlias".to_string(),
                    }),
                    "topLevelField".to_string(),
                    Some(SubSelection {
                        selections: vec![
                            NamedSelection::Quoted(
                                Alias {
                                    name: "nonIdentifier".to_string(),
                                },
                                "property name with spaces".to_string(),
                                None,
                            ),
                            NamedSelection::Path(
                                Alias {
                                    name: "pathSelection".to_string(),
                                },
                                PathSelection::from_slice(
                                    &[
                                        Key::Field("some".to_string()),
                                        Key::Field("nested".to_string()),
                                        Key::Field("path".to_string()),
                                    ],
                                    Some(SubSelection {
                                        selections: vec![
                                            NamedSelection::Field(
                                                Some(Alias {
                                                    name: "still".to_string(),
                                                }),
                                                "yet".to_string(),
                                                None,
                                            ),
                                            NamedSelection::Field(None, "more".to_string(), None,),
                                            NamedSelection::Field(
                                                None,
                                                "properties".to_string(),
                                                None,
                                            ),
                                        ],
                                        star: None,
                                    })
                                ),
                            ),
                            NamedSelection::Group(
                                Alias {
                                    name: "siblingGroup".to_string(),
                                },
                                SubSelection {
                                    selections: vec![
                                        NamedSelection::Field(None, "brother".to_string(), None,),
                                        NamedSelection::Field(None, "sister".to_string(), None,),
                                    ],
                                    star: None,
                                },
                            ),
                        ],
                        star: None,
                    }),
                )],
                star: None,
            }),
        );
    }

    fn check_path_selection(input: &str, expected: PathSelection) {
        assert_eq!(PathSelection::parse(input), Ok(("", expected.clone())));
        assert_eq!(selection!(input), JSONSelection::Path(expected.clone()));
    }

    #[test]
    fn test_path_selection() {
        check_path_selection(
            ".hello",
            PathSelection::from_slice(&[Key::Field("hello".to_string())], None),
        );

        {
            let expected = PathSelection::from_slice(
                &[
                    Key::Field("hello".to_string()),
                    Key::Field("world".to_string()),
                ],
                None,
            );
            check_path_selection(".hello.world", expected.clone());
            check_path_selection(".hello .world", expected.clone());
            check_path_selection(".hello. world", expected.clone());
            check_path_selection(".hello . world", expected.clone());
            check_path_selection("hello.world", expected.clone());
            check_path_selection("hello .world", expected.clone());
            check_path_selection("hello. world", expected.clone());
            check_path_selection("hello . world", expected.clone());
        }

        {
            let expected = PathSelection::from_slice(
                &[
                    Key::Field("hello".to_string()),
                    Key::Field("world".to_string()),
                ],
                Some(SubSelection {
                    selections: vec![NamedSelection::Field(None, "hello".to_string(), None)],
                    star: None,
                }),
            );
            check_path_selection(".hello.world { hello }", expected.clone());
            check_path_selection(".hello .world { hello }", expected.clone());
            check_path_selection(".hello. world { hello }", expected.clone());
            check_path_selection(".hello . world { hello }", expected.clone());
            check_path_selection(". hello.world { hello }", expected.clone());
            check_path_selection(". hello .world { hello }", expected.clone());
            check_path_selection(". hello. world { hello }", expected.clone());
            check_path_selection(". hello . world { hello }", expected.clone());
            check_path_selection("hello.world { hello }", expected.clone());
            check_path_selection("hello .world { hello }", expected.clone());
            check_path_selection("hello. world { hello }", expected.clone());
            check_path_selection("hello . world { hello }", expected.clone());
        }

        {
            let expected = PathSelection::from_slice(
                &[
                    Key::Field("nested".to_string()),
                    Key::Quoted("string literal".to_string()),
                    Key::Quoted("property".to_string()),
                    Key::Field("name".to_string()),
                ],
                None,
            );
            check_path_selection(
                ".nested.'string literal'.\"property\".name",
                expected.clone(),
            );
            check_path_selection(
                "nested.'string literal'.\"property\".name",
                expected.clone(),
            );
            check_path_selection(
                "nested. 'string literal'.\"property\".name",
                expected.clone(),
            );
            check_path_selection(
                "nested.'string literal'. \"property\".name",
                expected.clone(),
            );
            check_path_selection(
                "nested.'string literal'.\"property\" .name",
                expected.clone(),
            );
            check_path_selection(
                "nested.'string literal'.\"property\". name",
                expected.clone(),
            );
        }

        {
            let expected = PathSelection::from_slice(
                &[
                    Key::Field("nested".to_string()),
                    Key::Quoted("string literal".to_string()),
                ],
                Some(SubSelection {
                    selections: vec![NamedSelection::Quoted(
                        Alias {
                            name: "leggo".to_string(),
                        },
                        "my ego".to_string(),
                        None,
                    )],
                    star: None,
                }),
            );

            check_path_selection(
                ".nested.'string literal' { leggo: 'my ego' }",
                expected.clone(),
            );

            check_path_selection(
                "nested.'string literal' { leggo: 'my ego' }",
                expected.clone(),
            );

            check_path_selection(
                "nested. 'string literal' { leggo: 'my ego' }",
                expected.clone(),
            );

            check_path_selection(
                "nested . 'string literal' { leggo: 'my ego' }",
                expected.clone(),
            );
        }
    }

    #[test]
    fn test_path_selection_vars() {
        check_path_selection(
            "$var",
            PathSelection::Var("$var".to_string(), Box::new(PathSelection::Empty)),
        );

        check_path_selection(
            "$",
            PathSelection::Var("$".to_string(), Box::new(PathSelection::Empty)),
        );

        check_path_selection(
            "$var { hello }",
            PathSelection::Var(
                "$var".to_string(),
                Box::new(PathSelection::Selection(SubSelection {
                    selections: vec![NamedSelection::Field(None, "hello".to_string(), None)],
                    star: None,
                })),
            ),
        );

        check_path_selection(
            "$ { hello }",
            PathSelection::Var(
                "$".to_string(),
                Box::new(PathSelection::Selection(SubSelection {
                    selections: vec![NamedSelection::Field(None, "hello".to_string(), None)],
                    star: None,
                })),
            ),
        );

        check_path_selection(
            "$var { before alias: $args.arg after }",
            PathSelection::Var(
                "$var".to_string(),
                Box::new(PathSelection::Selection(SubSelection {
                    selections: vec![
                        NamedSelection::Field(None, "before".to_string(), None),
                        NamedSelection::Path(
                            Alias {
                                name: "alias".to_string(),
                            },
                            PathSelection::Var(
                                "$args".to_string(),
                                Box::new(PathSelection::Key(
                                    Key::Field("arg".to_string()),
                                    Box::new(PathSelection::Empty),
                                )),
                            ),
                        ),
                        NamedSelection::Field(None, "after".to_string(), None),
                    ],
                    star: None,
                })),
            ),
        );

        check_path_selection(
            "$.nested { key injected: $args.arg }",
            PathSelection::Var(
                "$".to_string(),
                Box::new(PathSelection::Key(
                    Key::Field("nested".to_string()),
                    Box::new(PathSelection::Selection(SubSelection {
                        selections: vec![
                            NamedSelection::Field(None, "key".to_string(), None),
                            NamedSelection::Path(
                                Alias {
                                    name: "injected".to_string(),
                                },
                                PathSelection::Var(
                                    "$args".to_string(),
                                    Box::new(PathSelection::Key(
                                        Key::Field("arg".to_string()),
                                        Box::new(PathSelection::Empty),
                                    )),
                                ),
                            ),
                        ],
                        star: None,
                    })),
                )),
            ),
        );

        check_path_selection(
            "$root.a.b.c",
            PathSelection::Var(
                "$root".to_string(),
                Box::new(PathSelection::from_slice(
                    &[
                        Key::Field("a".to_string()),
                        Key::Field("b".to_string()),
                        Key::Field("c".to_string()),
                    ],
                    None,
                )),
            ),
        );

        check_path_selection(
            "undotted.x.y.z",
            PathSelection::from_slice(
                &[
                    Key::Field("undotted".to_string()),
                    Key::Field("x".to_string()),
                    Key::Field("y".to_string()),
                    Key::Field("z".to_string()),
                ],
                None,
            ),
        );

        check_path_selection(
            ".dotted.x.y.z",
            PathSelection::from_slice(
                &[
                    Key::Field("dotted".to_string()),
                    Key::Field("x".to_string()),
                    Key::Field("y".to_string()),
                    Key::Field("z".to_string()),
                ],
                None,
            ),
        );

        check_path_selection(
            "$.data",
            PathSelection::Var(
                "$".to_string(),
                Box::new(PathSelection::Key(
                    Key::Field("data".to_string()),
                    Box::new(PathSelection::Empty),
                )),
            ),
        );

        check_path_selection(
            "$.data.'quoted property'.nested",
            PathSelection::Var(
                "$".to_string(),
                Box::new(PathSelection::Key(
                    Key::Field("data".to_string()),
                    Box::new(PathSelection::Key(
                        Key::Quoted("quoted property".to_string()),
                        Box::new(PathSelection::Key(
                            Key::Field("nested".to_string()),
                            Box::new(PathSelection::Empty),
                        )),
                    )),
                )),
            ),
        );

        assert_eq!(
            PathSelection::parse("naked"),
            Err(nom::Err::Error(nom::error::Error::new(
                "",
                nom::error::ErrorKind::IsNot,
            ))),
        );

        assert_eq!(
            PathSelection::parse("naked { hi }"),
            Err(nom::Err::Error(nom::error::Error::new(
                "",
                nom::error::ErrorKind::IsNot,
            ))),
        );

        assert_eq!(
            PathSelection::parse("valid.$invalid"),
            Err(nom::Err::Error(nom::error::Error::new(
                ".$invalid",
                nom::error::ErrorKind::IsNot,
            ))),
        );

        assert_eq!(
            selection!("$"),
            JSONSelection::Path(PathSelection::Var(
                "$".to_string(),
                Box::new(PathSelection::Empty),
            )),
        );

        assert_eq!(
            selection!("$this"),
            JSONSelection::Path(PathSelection::Var(
                "$this".to_string(),
                Box::new(PathSelection::Empty),
            )),
        );
    }

    #[test]
    fn test_subselection() {
        assert_eq!(
            SubSelection::parse(" { \n } "),
            Ok((
                "",
                SubSelection {
                    selections: vec![],
                    star: None,
                },
            )),
        );

        assert_eq!(
            SubSelection::parse("{hello}"),
            Ok((
                "",
                SubSelection {
                    selections: vec![NamedSelection::Field(None, "hello".to_string(), None),],
                    star: None,
                },
            )),
        );

        assert_eq!(
            SubSelection::parse("{ hello }"),
            Ok((
                "",
                SubSelection {
                    selections: vec![NamedSelection::Field(None, "hello".to_string(), None),],
                    star: None,
                },
            )),
        );

        assert_eq!(
            SubSelection::parse("  { padded  } "),
            Ok((
                "",
                SubSelection {
                    selections: vec![NamedSelection::Field(None, "padded".to_string(), None),],
                    star: None,
                },
            )),
        );

        assert_eq!(
            SubSelection::parse("{ hello world }"),
            Ok((
                "",
                SubSelection {
                    selections: vec![
                        NamedSelection::Field(None, "hello".to_string(), None),
                        NamedSelection::Field(None, "world".to_string(), None),
                    ],
                    star: None,
                },
            )),
        );

        assert_eq!(
            SubSelection::parse("{ hello { world } }"),
            Ok((
                "",
                SubSelection {
                    selections: vec![NamedSelection::Field(
                        None,
                        "hello".to_string(),
                        Some(SubSelection {
                            selections: vec![NamedSelection::Field(
                                None,
                                "world".to_string(),
                                None
                            ),],
                            star: None,
                        })
                    ),],
                    star: None,
                },
            )),
        );
    }

    #[test]
    fn test_star_selection() {
        assert_eq!(
            StarSelection::parse("rest: *"),
            Ok((
                "",
                StarSelection(
                    Some(Alias {
                        name: "rest".to_string(),
                    }),
                    None
                ),
            )),
        );

        assert_eq!(
            StarSelection::parse("*"),
            Ok(("", StarSelection(None, None),)),
        );

        assert_eq!(
            StarSelection::parse(" * "),
            Ok(("", StarSelection(None, None),)),
        );

        assert_eq!(
            StarSelection::parse(" * { hello } "),
            Ok((
                "",
                StarSelection(
                    None,
                    Some(Box::new(SubSelection {
                        selections: vec![NamedSelection::Field(None, "hello".to_string(), None),],
                        star: None,
                    }))
                ),
            )),
        );

        assert_eq!(
            StarSelection::parse("hi: * { hello }"),
            Ok((
                "",
                StarSelection(
                    Some(Alias {
                        name: "hi".to_string(),
                    }),
                    Some(Box::new(SubSelection {
                        selections: vec![NamedSelection::Field(None, "hello".to_string(), None),],
                        star: None,
                    }))
                ),
            )),
        );

        assert_eq!(
            StarSelection::parse("alias: * { x y z rest: * }"),
            Ok((
                "",
                StarSelection(
                    Some(Alias {
                        name: "alias".to_string()
                    }),
                    Some(Box::new(SubSelection {
                        selections: vec![
                            NamedSelection::Field(None, "x".to_string(), None),
                            NamedSelection::Field(None, "y".to_string(), None),
                            NamedSelection::Field(None, "z".to_string(), None),
                        ],
                        star: Some(StarSelection(
                            Some(Alias {
                                name: "rest".to_string(),
                            }),
                            None
                        )),
                    })),
                ),
            )),
        );

        assert_eq!(
            selection!(" before alias: * { * { a b c } } "),
            JSONSelection::Named(SubSelection {
                selections: vec![NamedSelection::Field(None, "before".to_string(), None),],
                star: Some(StarSelection(
                    Some(Alias {
                        name: "alias".to_string()
                    }),
                    Some(Box::new(SubSelection {
                        selections: vec![],
                        star: Some(StarSelection(
                            None,
                            Some(Box::new(SubSelection {
                                selections: vec![
                                    NamedSelection::Field(None, "a".to_string(), None),
                                    NamedSelection::Field(None, "b".to_string(), None),
                                    NamedSelection::Field(None, "c".to_string(), None),
                                ],
                                star: None,
                            }))
                        )),
                    })),
                )),
            }),
        );

        assert_eq!(
            selection!(" before group: { * { a b c } } after "),
            JSONSelection::Named(SubSelection {
                selections: vec![
                    NamedSelection::Field(None, "before".to_string(), None),
                    NamedSelection::Group(
                        Alias {
                            name: "group".to_string(),
                        },
                        SubSelection {
                            selections: vec![],
                            star: Some(StarSelection(
                                None,
                                Some(Box::new(SubSelection {
                                    selections: vec![
                                        NamedSelection::Field(None, "a".to_string(), None),
                                        NamedSelection::Field(None, "b".to_string(), None),
                                        NamedSelection::Field(None, "c".to_string(), None),
                                    ],
                                    star: None,
                                }))
                            )),
                        },
                    ),
                    NamedSelection::Field(None, "after".to_string(), None),
                ],
                star: None,
            }),
        );
    }
}
