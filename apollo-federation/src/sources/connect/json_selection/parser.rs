use std::fmt::Display;

use nom::branch::alt;
use nom::character::complete::char;
use nom::character::complete::one_of;
use nom::combinator::all_consuming;
use nom::combinator::map;
use nom::combinator::opt;
use nom::combinator::recognize;
use nom::multi::many0;
use nom::multi::many1;
use nom::sequence::pair;
use nom::sequence::preceded;
use nom::sequence::tuple;
use nom::IResult;
use serde::Serialize;

use super::helpers::spaces_or_comments;

// Selection ::= NamedSelection* StarSelection? | PathSelection

#[derive(Debug, PartialEq, Clone, Serialize)]
pub enum Selection {
    // Although we reuse the SubSelection type for the Selection::Named case, we
    // parse it as a sequence of NamedSelection items without the {...} curly
    // braces that SubSelection::parse expects.
    Named(SubSelection),
    Path(PathSelection),
}

impl Selection {
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
}

// NamedSelection ::=
//     | Alias? Identifier SubSelection?
//     | Alias StringLiteral SubSelection?
//     | Alias PathSelection
//     | Alias SubSelection

#[derive(Debug, PartialEq, Clone, Serialize)]
pub enum NamedSelection {
    Field(Option<Alias>, String, Option<SubSelection>),
    Quoted(Alias, String, Option<SubSelection>),
    Path(Alias, PathSelection),
    Group(Alias, SubSelection),
}

impl NamedSelection {
    fn parse(input: &str) -> IResult<&str, Self> {
        alt((
            Self::parse_field,
            Self::parse_quoted,
            Self::parse_path,
            Self::parse_group,
        ))(input)
    }

    fn parse_field(input: &str) -> IResult<&str, Self> {
        tuple((
            opt(Alias::parse),
            parse_identifier,
            opt(SubSelection::parse),
        ))(input)
        .map(|(input, (alias, name, selection))| (input, Self::Field(alias, name, selection)))
    }

    fn parse_quoted(input: &str) -> IResult<&str, Self> {
        tuple((Alias::parse, parse_string_literal, opt(SubSelection::parse)))(input)
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
    pub(crate) fn property_path(&self) -> Vec<Property> {
        match self {
            NamedSelection::Field(_, name, _) => vec![Property::Field(name.to_string())],
            NamedSelection::Quoted(_, _, Some(_)) => todo!(),
            NamedSelection::Quoted(_, name, None) => vec![Property::Quoted(name.to_string())],
            NamedSelection::Path(_, path) => path.collect_paths(),
            NamedSelection::Group(alias, _) => vec![Property::Field(alias.name.to_string())],
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
}

// PathSelection ::= ("." Property)+ SubSelection?

#[derive(Debug, PartialEq, Clone, Serialize)]
pub enum PathSelection {
    // We use a recursive structure here instead of a Vec<Property> to make
    // applying the selection to a JSON value easier.
    Path(Property, Box<PathSelection>),
    Selection(SubSelection),
    Empty,
}

impl PathSelection {
    fn parse(input: &str) -> IResult<&str, Self> {
        tuple((
            spaces_or_comments,
            many1(preceded(char('.'), Property::parse)),
            opt(SubSelection::parse),
            spaces_or_comments,
        ))(input)
        .map(|(input, (_, path, selection, _))| (input, Self::from_slice(&path, selection)))
    }

    fn from_slice(properties: &[Property], selection: Option<SubSelection>) -> Self {
        match properties {
            [] => selection.map_or(Self::Empty, Self::Selection),
            [head, tail @ ..] => {
                Self::Path(head.clone(), Box::new(Self::from_slice(tail, selection)))
            }
        }
    }

    /// Collect all nested paths
    ///
    /// This method attempts to collect as many paths as possible, shorting out once
    /// a non path selection is encountered.
    pub(crate) fn collect_paths(&self) -> Vec<Property> {
        let mut results = Vec::new();

        // Collect as many as possible
        let mut current = self;
        while let Self::Path(prop, rest) = current {
            results.push(prop.clone());

            current = rest;
        }

        results
    }

    /// Find the next subselection, traversing nested chains if needed
    pub(crate) fn next_subselection(&self) -> Option<&SubSelection> {
        match self {
            PathSelection::Path(_, path) => path.next_subselection(),
            PathSelection::Selection(sub) => Some(sub),
            PathSelection::Empty => None,
        }
    }
}

// SubSelection ::= "{" NamedSelection* StarSelection? "}"

#[derive(Debug, PartialEq, Clone, Serialize)]
pub struct SubSelection {
    pub selections: Vec<NamedSelection>,
    pub star: Option<StarSelection>,
}

impl SubSelection {
    fn parse(input: &str) -> IResult<&str, Self> {
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
}

// StarSelection ::= Alias? "*" SubSelection?

#[derive(Debug, PartialEq, Clone, Serialize)]
pub struct StarSelection(Option<Alias>, Option<Box<SubSelection>>);

impl StarSelection {
    fn parse(input: &str) -> IResult<&str, Self> {
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
    name: String,
}

impl Alias {
    fn parse(input: &str) -> IResult<&str, Self> {
        tuple((parse_identifier, char(':'), spaces_or_comments))(input)
            .map(|(input, (name, _, _))| (input, Self { name }))
    }
}

// Property ::= Identifier | StringLiteral

#[derive(Debug, PartialEq, Eq, Hash, Clone, Serialize)]
pub enum Property {
    Field(String),
    Quoted(String),
    Index(usize),
}

impl Property {
    fn parse(input: &str) -> IResult<&str, Self> {
        alt((
            map(parse_identifier, Self::Field),
            map(parse_string_literal, Self::Quoted),
        ))(input)
    }
}

impl Display for Property {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Property::Field(field) => write!(f, ".{field}"),
            Property::Quoted(quote) => write!(f, r#"."{quote}""#),
            Property::Index(index) => write!(f, "[{index}]"),
        }
    }
}

// Identifier ::= [a-zA-Z_][0-9a-zA-Z_]*

fn parse_identifier(input: &str) -> IResult<&str, String> {
    tuple((
        spaces_or_comments,
        recognize(pair(
            one_of("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ_"),
            many0(one_of(
                "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ_0123456789",
            )),
        )),
        spaces_or_comments,
    ))(input)
    .map(|(input, (_, name, _))| (input, name.to_string()))
}

// StringLiteral ::=
//     | "'" ("\'" | [^'])* "'"
//     | '"' ('\"' | [^"])* '"'

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

// GraphQL Selection Set -------------------------------------------------------

use apollo_compiler::ast;
use apollo_compiler::ast::Selection as GraphQLSelection;

#[derive(Default)]
struct GraphQLSelections(Vec<Result<GraphQLSelection, String>>);

impl GraphQLSelections {
    fn valid_selections(self) -> Vec<GraphQLSelection> {
        self.0.into_iter().filter_map(|i| i.ok()).collect()
    }
}

impl From<Vec<GraphQLSelection>> for GraphQLSelections {
    fn from(val: Vec<GraphQLSelection>) -> Self {
        Self(val.into_iter().map(Ok).collect())
    }
}

impl From<Selection> for Vec<GraphQLSelection> {
    fn from(val: Selection) -> Vec<GraphQLSelection> {
        match val {
            Selection::Named(named_selections) => {
                GraphQLSelections::from(named_selections).valid_selections()
            }
            Selection::Path(path_selection) => path_selection.into(),
        }
    }
}

fn new_field(name: String, selection: Option<GraphQLSelections>) -> GraphQLSelection {
    GraphQLSelection::Field(
        apollo_compiler::ast::Field {
            alias: None,
            name: ast::Name::new_unchecked(name.into()),
            arguments: Default::default(),
            directives: Default::default(),
            selection_set: selection
                .map(GraphQLSelections::valid_selections)
                .unwrap_or_default(),
        }
        .into(),
    )
}

impl From<NamedSelection> for Vec<GraphQLSelection> {
    fn from(val: NamedSelection) -> Vec<GraphQLSelection> {
        match val {
            NamedSelection::Field(alias, name, selection) => vec![new_field(
                alias.map(|a| a.name).unwrap_or(name),
                selection.map(|s| s.into()),
            )],
            NamedSelection::Quoted(alias, _name, selection) => {
                vec![new_field(
                    alias.name,
                    selection.map(GraphQLSelections::from),
                )]
            }
            NamedSelection::Path(alias, path_selection) => {
                let graphql_selection: Vec<GraphQLSelection> = path_selection.into();
                vec![new_field(
                    alias.name,
                    Some(GraphQLSelections::from(graphql_selection)),
                )]
            }
            NamedSelection::Group(alias, sub_selection) => {
                vec![new_field(alias.name, Some(sub_selection.into()))]
            }
        }
    }
}

impl From<PathSelection> for Vec<GraphQLSelection> {
    fn from(val: PathSelection) -> Vec<GraphQLSelection> {
        match val {
            PathSelection::Path(_head, tail) => {
                let tail = *tail;
                tail.into()
            }
            PathSelection::Selection(selection) => {
                GraphQLSelections::from(selection).valid_selections()
            }
            PathSelection::Empty => vec![],
        }
    }
}

impl From<SubSelection> for GraphQLSelections {
    // give as much as we can, yield errors for star selection without alias.
    fn from(val: SubSelection) -> GraphQLSelections {
        let mut selections = val
            .selections
            .into_iter()
            .flat_map(|named_selection| {
                let selections: Vec<GraphQLSelection> = named_selection.into();
                GraphQLSelections::from(selections).0
            })
            .collect::<Vec<Result<GraphQLSelection, String>>>();

        if let Some(StarSelection(alias, sub_selection)) = val.star {
            if let Some(alias) = alias {
                let star = new_field(
                    alias.name,
                    sub_selection.map(|s| GraphQLSelections::from(*s)),
                );
                selections.push(Ok(star));
            } else {
                selections.push(Err(
                    "star selection without alias cannot be converted to GraphQL".to_string(),
                ));
            }
        }
        GraphQLSelections(selections)
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
    fn test_property() {
        assert_eq!(
            Property::parse("hello"),
            Ok(("", Property::Field("hello".to_string()))),
        );

        assert_eq!(
            Property::parse("'hello'"),
            Ok(("", Property::Quoted("hello".to_string()))),
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
                Selection::Named(SubSelection {
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
            Selection::Named(SubSelection {
                selections: vec![],
                star: None,
            }),
        );

        assert_eq!(
            selection!("   "),
            Selection::Named(SubSelection {
                selections: vec![],
                star: None,
            }),
        );

        assert_eq!(
            selection!("hello"),
            Selection::Named(SubSelection {
                selections: vec![NamedSelection::Field(None, "hello".to_string(), None),],
                star: None,
            }),
        );

        assert_eq!(
            selection!(".hello"),
            Selection::Path(PathSelection::from_slice(
                &[Property::Field("hello".to_string()),],
                None
            )),
        );

        assert_eq!(
            selection!("hi: .hello.world"),
            Selection::Named(SubSelection {
                selections: vec![NamedSelection::Path(
                    Alias {
                        name: "hi".to_string(),
                    },
                    PathSelection::from_slice(
                        &[
                            Property::Field("hello".to_string()),
                            Property::Field("world".to_string()),
                        ],
                        None
                    ),
                )],
                star: None,
            }),
        );

        assert_eq!(
            selection!("before hi: .hello.world after"),
            Selection::Named(SubSelection {
                selections: vec![
                    NamedSelection::Field(None, "before".to_string(), None),
                    NamedSelection::Path(
                        Alias {
                            name: "hi".to_string(),
                        },
                        PathSelection::from_slice(
                            &[
                                Property::Field("hello".to_string()),
                                Property::Field("world".to_string()),
                            ],
                            None
                        ),
                    ),
                    NamedSelection::Field(None, "after".to_string(), None),
                ],
                star: None,
            }),
        );

        let before_path_nested_after_result = Selection::Named(SubSelection {
            selections: vec![
                NamedSelection::Field(None, "before".to_string(), None),
                NamedSelection::Path(
                    Alias {
                        name: "hi".to_string(),
                    },
                    PathSelection::from_slice(
                        &[
                            Property::Field("hello".to_string()),
                            Property::Field("world".to_string()),
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
            before_path_nested_after_result,
        );

        assert_eq!(
            selection!("before hi:.hello.world{nested names}after"),
            before_path_nested_after_result,
        );

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
            Selection::Named(SubSelection {
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
                                        Property::Field("some".to_string()),
                                        Property::Field("nested".to_string()),
                                        Property::Field("path".to_string()),
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

    #[test]
    fn test_path_selection() {
        fn check_path_selection(input: &str, expected: PathSelection) {
            assert_eq!(PathSelection::parse(input), Ok(("", expected.clone())));
            assert_eq!(selection!(input), Selection::Path(expected.clone()));
        }

        check_path_selection(
            ".hello",
            PathSelection::from_slice(&[Property::Field("hello".to_string())], None),
        );

        check_path_selection(
            ".hello.world",
            PathSelection::from_slice(
                &[
                    Property::Field("hello".to_string()),
                    Property::Field("world".to_string()),
                ],
                None,
            ),
        );

        check_path_selection(
            ".hello.world { hello }",
            PathSelection::from_slice(
                &[
                    Property::Field("hello".to_string()),
                    Property::Field("world".to_string()),
                ],
                Some(SubSelection {
                    selections: vec![NamedSelection::Field(None, "hello".to_string(), None)],
                    star: None,
                }),
            ),
        );

        check_path_selection(
            ".nested.'string literal'.\"property\".name",
            PathSelection::from_slice(
                &[
                    Property::Field("nested".to_string()),
                    Property::Quoted("string literal".to_string()),
                    Property::Quoted("property".to_string()),
                    Property::Field("name".to_string()),
                ],
                None,
            ),
        );

        check_path_selection(
            ".nested.'string literal' { leggo: 'my ego' }",
            PathSelection::from_slice(
                &[
                    Property::Field("nested".to_string()),
                    Property::Quoted("string literal".to_string()),
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
            ),
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
            Selection::Named(SubSelection {
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
            Selection::Named(SubSelection {
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

    use apollo_compiler::ast::Selection as GraphQLSelection;

    fn print_set(set: &[apollo_compiler::ast::Selection]) -> String {
        set.iter()
            .map(|s| s.serialize().to_string())
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn into_selection_set() {
        let selection = selection!("f");
        let set: Vec<GraphQLSelection> = selection.into();
        assert_eq!(print_set(&set), "f");

        let selection = selection!("f f2 f3");
        let set: Vec<GraphQLSelection> = selection.into();
        assert_eq!(print_set(&set), "f f2 f3");

        let selection = selection!("f { f2 f3 }");
        let set: Vec<GraphQLSelection> = selection.into();
        assert_eq!(print_set(&set), "f {\n  f2\n  f3\n}");

        let selection = selection!("a: f { b: f2 }");
        let set: Vec<GraphQLSelection> = selection.into();
        assert_eq!(print_set(&set), "a {\n  b\n}");

        let selection = selection!(".a { b c }");
        let set: Vec<GraphQLSelection> = selection.into();
        assert_eq!(print_set(&set), "b c");

        let selection = selection!(".a.b { c: .d e }");
        let set: Vec<GraphQLSelection> = selection.into();
        assert_eq!(print_set(&set), "c e");

        let selection = selection!("a: { b c }");
        let set: Vec<GraphQLSelection> = selection.into();
        assert_eq!(print_set(&set), "a {\n  b\n  c\n}");

        let selection = selection!("a: 'quoted'");
        let set: Vec<GraphQLSelection> = selection.into();
        assert_eq!(print_set(&set), "a");

        let selection = selection!("a b: *");
        let set: Vec<GraphQLSelection> = selection.into();
        assert_eq!(print_set(&set), "a b");

        let selection = selection!("a *");
        let set: Vec<GraphQLSelection> = selection.into();
        assert_eq!(print_set(&set), "a");
    }
}
