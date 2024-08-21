use std::fmt::Display;

use nom::branch::alt;
use nom::bytes::complete::tag;
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
use serde_json_bytes::Value as JSON;

use super::helpers::spaces_or_comments;
use super::known_var::KnownVariable;
use super::lit_expr::LitExpr;

pub(crate) trait ExternalVarPaths {
    fn external_var_paths(&self) -> Vec<&PathSelection>;
}

// JSONSelection     ::= NakedSubSelection | PathSelection
// NakedSubSelection ::= NamedSelection* StarSelection?

#[derive(Debug, PartialEq, Clone)]
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
            JSONSelection::Path(path) => path.path == PathList::Empty,
        }
    }

    pub fn parse(input: &str) -> IResult<&str, Self> {
        alt((
            all_consuming(map(SubSelection::parse_naked, Self::Named)),
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

impl ExternalVarPaths for JSONSelection {
    fn external_var_paths(&self) -> Vec<&PathSelection> {
        match self {
            JSONSelection::Named(subselect) => subselect.external_var_paths(),
            JSONSelection::Path(path) => path.external_var_paths(),
        }
    }
}

// NamedSelection       ::= NamedPathSelection | NamedFieldSelection | NamedGroupSelection
// NamedPathSelection   ::= Alias PathSelection
// NamedFieldSelection  ::= Alias? Key SubSelection?
// NamedGroupSelection  ::= Alias SubSelection

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum NamedSelection {
    Field(Option<Alias>, Key, Option<SubSelection>),
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
            Self::parse_group,
        ))(input)
    }

    fn parse_field(input: &str) -> IResult<&str, Self> {
        tuple((
            opt(Alias::parse),
            delimited(spaces_or_comments, Key::parse, spaces_or_comments),
            opt(SubSelection::parse),
        ))(input)
        .map(|(input, (alias, name, selection))| (input, Self::Field(alias, name, selection)))
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
            Self::Path(alias, _) => alias.name.as_str(),
            Self::Group(alias, _) => alias.name.as_str(),
        }
    }

    /// Find the next subselection, if present
    pub(crate) fn next_subselection(&self) -> Option<&SubSelection> {
        match self {
            // Paths are complicated because they can have a subselection deeply nested
            Self::Path(_, path) => path.next_subselection(),

            // The other options have it at the root
            Self::Field(_, _, Some(sub)) | Self::Group(_, sub) => Some(sub),

            // Every other option does not have a subselection
            _ => None,
        }
    }

    pub(crate) fn next_mut_subselection(&mut self) -> Option<&mut SubSelection> {
        match self {
            // Paths are complicated because they can have a subselection deeply nested
            Self::Path(_, path) => path.next_mut_subselection(),

            // The other options have it at the root
            Self::Field(_, _, Some(sub)) | Self::Group(_, sub) => Some(sub),

            // Every other option does not have a subselection
            _ => None,
        }
    }
}

impl ExternalVarPaths for NamedSelection {
    fn external_var_paths(&self) -> Vec<&PathSelection> {
        match self {
            Self::Field(_, _, Some(sub)) | Self::Group(_, sub) => sub.external_var_paths(),
            Self::Path(_, path) => path.external_var_paths(),
            _ => vec![],
        }
    }
}

// PathSelection ::= (VarPath | KeyPath | AtPath) SubSelection?
// VarPath       ::= "$" (NO_SPACE Identifier)? PathStep*
// KeyPath       ::= Key PathStep+
// AtPath        ::= "@" PathStep*
// PathStep      ::= "." Key | "->" Identifier MethodArgs?

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct PathSelection {
    pub(super) path: PathList,
}

impl PathSelection {
    pub fn parse(input: &str) -> IResult<&str, Self> {
        let (input, path) = PathList::parse(input)?;
        Ok((input, Self { path }))
    }

    pub(crate) fn var_name_and_nested_keys(&self) -> Option<(&KnownVariable, Vec<&str>)> {
        match &self.path {
            PathList::Var(var_name, tail) => Some((var_name, tail.prefix_of_keys())),
            _ => None,
        }
    }

    pub(super) fn is_single_key(&self) -> bool {
        self.path.is_single_key()
    }

    pub(super) fn from_slice(keys: &[Key], selection: Option<SubSelection>) -> Self {
        Self {
            path: PathList::from_slice(keys, selection),
        }
    }

    pub(super) fn next_subselection(&self) -> Option<&SubSelection> {
        self.path.next_subselection()
    }

    pub(super) fn next_mut_subselection(&mut self) -> Option<&mut SubSelection> {
        self.path.next_mut_subselection()
    }
}

impl ExternalVarPaths for PathSelection {
    fn external_var_paths(&self) -> Vec<&PathSelection> {
        let mut paths = vec![];
        match &self.path {
            PathList::Var(var_name, tail) => {
                // The $ and @ variables refer to parts of the current JSON
                // data, so they do not need to be surfaced as external variable
                // references.
                if var_name != &KnownVariable::Dollar && var_name != &KnownVariable::AtSign {
                    paths.push(self);
                }
                paths.extend(tail.external_var_paths());
            }
            PathList::Key(_, tail) => {
                paths.extend(tail.external_var_paths());
            }
            PathList::Method(_, opt_args, tail) => {
                if let Some(args) = opt_args {
                    for lit_arg in &args.0 {
                        paths.extend(lit_arg.external_var_paths());
                    }
                }
                paths.extend(tail.external_var_paths());
            }
            PathList::Selection(sub) => paths.extend(sub.external_var_paths()),
            PathList::Empty => {}
        };
        paths
    }
}

impl From<PathList> for PathSelection {
    fn from(path: PathList) -> Self {
        Self { path }
    }
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub(super) enum PathList {
    // A VarPath must start with a variable (either $identifier, $, or @),
    // followed by any number of PathStep items (the Box<PathList>). Because we
    // represent the @ quasi-variable using PathList::Var, this variant handles
    // both VarPath and AtPath from the grammar. The String variable name must
    // always contain the $ character. The PathList::Var variant may only appear
    // at the beginning of a PathSelection's PathList, not in the middle.
    Var(KnownVariable, Box<PathList>),

    // A PathSelection that starts with a PathList::Key is a KeyPath, but a
    // PathList::Key also counts as PathStep item, so it may also appear in the
    // middle/tail of a PathList.
    Key(Key, Box<PathList>),

    // A PathList::Method is a PathStep item that may appear only in the
    // middle/tail (not the beginning) of a PathSelection. Methods are
    // distinguished from .keys by their ->method invocation syntax.
    Method(String, Option<MethodArgs>, Box<PathList>),

    // Optionally, a PathList may end with a SubSelection, which applies a set
    // of named selections to the final value of the path. PathList::Selection
    // by itself is not a valid PathList.
    Selection(SubSelection),

    // Every PathList must be terminated by either PathList::Selection or
    // PathList::Empty. PathList::Empty by itself is not a valid PathList.
    Empty,
}

impl PathList {
    pub fn parse(input: &str) -> IResult<&str, Self> {
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

        // Variable references (including @ references) and key references
        // without a leading . are accepted only at depth 0, or at the beginning
        // of the PathSelection.
        if depth == 0 {
            if let Ok((suffix, opt_var)) = delimited(
                tuple((spaces_or_comments, char('$'))),
                opt(parse_identifier_no_space),
                spaces_or_comments,
            )(input)
            {
                let (input, rest) = Self::parse_with_depth(suffix, depth + 1)?;
                // Note the $ prefix is included in the variable name.
                let var_name = format!("${}", opt_var.unwrap_or("".to_string()));
                return if let Some(known_var) = KnownVariable::from_str(&var_name) {
                    Ok((input, Self::Var(known_var, Box::new(rest))))
                } else {
                    // Reject unknown variables at parse time.
                    // TODO Improve these parse error messages.
                    Err(nom::Err::Error(nom::error::Error::new(
                        input,
                        nom::error::ErrorKind::IsNot,
                    )))
                };
            }

            if let Ok((suffix, _)) =
                tuple((spaces_or_comments, char('@'), spaces_or_comments))(input)
            {
                let (input, rest) = Self::parse_with_depth(suffix, depth + 1)?;
                // Because we include the $ in the variable name for ordinary
                // variables, we have the freedom to store other symbols as
                // special variables, such as @ for the current value. In fact,
                // as long as we can parse the token(s) as a PathList::Var, the
                // name of a variable could technically be any string we like.
                return Ok((input, Self::Var(KnownVariable::AtSign, Box::new(rest))));
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

        // PathSelection can never start with a naked ->method (instead, use
        // $->method if you want to operate on the current value).
        if let Ok((suffix, (method, args))) = preceded(
            tuple((spaces_or_comments, tag("->"), spaces_or_comments)),
            tuple((parse_identifier, opt(MethodArgs::parse))),
        )(input)
        {
            let (input, rest) = Self::parse_with_depth(suffix, depth + 1)?;
            return Ok((input, Self::Method(method, args, Box::new(rest))));
        }

        // Likewise, if the PathSelection has a SubSelection, it must appear at
        // the end of a non-empty path.
        if let Ok((suffix, selection)) = SubSelection::parse(input) {
            return Ok((suffix, Self::Selection(selection)));
        }

        // The Self::Empty enum case is used to indicate the end of a
        // PathSelection that has no SubSelection.
        Ok((input, Self::Empty))
    }

    pub(super) fn is_single_key(&self) -> bool {
        match self {
            Self::Key(_, rest) => matches!(rest.as_ref(), Self::Selection(_) | Self::Empty),
            _ => false,
        }
    }

    fn prefix_of_keys(&self) -> Vec<&str> {
        match self {
            Self::Key(key, rest) => {
                let mut keys = vec![key.as_str()];
                keys.extend(rest.prefix_of_keys());
                keys
            }
            _ => vec![],
        }
    }

    pub(super) fn from_slice(properties: &[Key], selection: Option<SubSelection>) -> Self {
        match properties {
            [] => selection.map_or(Self::Empty, Self::Selection),
            [head, tail @ ..] => {
                Self::Key(head.clone(), Box::new(Self::from_slice(tail, selection)))
            }
        }
    }

    /// Find the next subselection, traversing nested chains if needed
    pub(super) fn next_subselection(&self) -> Option<&SubSelection> {
        match self {
            Self::Var(_, tail) => tail.next_subselection(),
            Self::Key(_, tail) => tail.next_subselection(),
            Self::Method(_, _, tail) => tail.next_subselection(),
            Self::Selection(sub) => Some(sub),
            Self::Empty => None,
        }
    }

    /// Find the next subselection, traversing nested chains if needed. Returns a mutable reference
    pub(super) fn next_mut_subselection(&mut self) -> Option<&mut SubSelection> {
        match self {
            Self::Var(_, tail) => tail.next_mut_subselection(),
            Self::Key(_, tail) => tail.next_mut_subselection(),
            Self::Method(_, _, tail) => tail.next_mut_subselection(),
            Self::Selection(sub) => Some(sub),
            Self::Empty => None,
        }
    }
}

impl ExternalVarPaths for PathList {
    fn external_var_paths(&self) -> Vec<&PathSelection> {
        let mut paths = vec![];
        match self {
            // PathSelection::external_var_paths is responsible for adding all
            // variable &PathSelection items to the set, since this
            // PathList::Var case cannot be sure it's looking at the beginning
            // of the path. However, we call rest.external_var_paths()
            // recursively because the tail of the list could contain other full
            // PathSelection variable references.
            PathList::Var(_, rest) | PathList::Key(_, rest) => {
                paths.extend(rest.external_var_paths());
            }
            PathList::Method(_, opt_args, rest) => {
                if let Some(args) = opt_args {
                    for lit_arg in &args.0 {
                        paths.extend(lit_arg.external_var_paths());
                    }
                }
                paths.extend(rest.external_var_paths());
            }
            PathList::Selection(sub) => paths.extend(sub.external_var_paths()),
            PathList::Empty => {}
        }
        paths
    }
}

// SubSelection ::= "{" NakedSubSelection "}"

#[derive(Debug, PartialEq, Eq, Clone, Default)]
pub struct SubSelection {
    pub(super) selections: Vec<NamedSelection>,
    pub(super) star: Option<StarSelection>,
}

impl SubSelection {
    pub(crate) fn parse(input: &str) -> IResult<&str, Self> {
        delimited(
            tuple((spaces_or_comments, char('{'))),
            Self::parse_naked,
            tuple((char('}'), spaces_or_comments)),
        )(input)
    }

    fn parse_naked(input: &str) -> IResult<&str, Self> {
        tuple((
            spaces_or_comments,
            many0(NamedSelection::parse),
            // Note that when a * selection is used, it must be the last
            // selection in the SubSelection, since it does not count as a
            // NamedSelection, and is stored as a separate field from the
            // selections vector.
            opt(StarSelection::parse),
            spaces_or_comments,
        ))(input)
        .map(|(input, (_, selections, star, _))| (input, Self { selections, star }))
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

impl ExternalVarPaths for SubSelection {
    fn external_var_paths(&self) -> Vec<&PathSelection> {
        let mut paths = vec![];
        for selection in &self.selections {
            paths.extend(selection.external_var_paths());
        }
        paths
    }
}

// StarSelection ::= Alias? "*" SubSelection?

#[derive(Debug, PartialEq, Eq, Clone)]
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

// Alias ::= Key ":"

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct Alias {
    pub(super) name: Key,
}

impl Alias {
    pub fn new(name: &str) -> Self {
        Self {
            name: Key::field(name),
        }
    }

    pub fn quoted(name: &str) -> Self {
        Self {
            name: Key::quoted(name),
        }
    }

    fn parse(input: &str) -> IResult<&str, Self> {
        tuple((
            Key::parse,
            char(':'),
            spaces_or_comments,
        ))(input)
        .map(|(input, (name, _, _))| (input, Self { name }))
    }

    pub fn name(&self) -> &str {
        self.name.as_str()
    }
}

// Key ::= Identifier | LitString

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum Key {
    Field(String),
    Quoted(String),
}

impl Key {
    pub fn parse(input: &str) -> IResult<&str, Self> {
        alt((
            map(parse_identifier, Self::Field),
            map(parse_string_literal, Self::Quoted),
        ))(input)
    }

    pub fn field(name: &str) -> Self {
        Self::Field(name.to_string())
    }

    pub fn quoted(name: &str) -> Self {
        Self::Quoted(name.to_string())
    }

    pub fn is_quoted(&self) -> bool {
        matches!(self, Self::Quoted(_))
    }

    pub fn to_json(&self) -> JSON {
        match self {
            Key::Field(name) => JSON::String(name.clone().into()),
            Key::Quoted(name) => JSON::String(name.clone().into()),
        }
    }

    // This method returns the field/property name as a String, and is
    // appropriate for accessing JSON properties, in contrast to the dotted
    // method below.
    pub fn as_string(&self) -> String {
        match self {
            Key::Field(name) => name.clone(),
            Key::Quoted(name) => name.clone(),
        }
    }
    // Like as_string, but without cloning a new String, for times when the Key
    // itself lives longer than the &str.
    pub fn as_str(&self) -> &str {
        match self {
            Key::Field(name) => name.as_str(),
            Key::Quoted(name) => name.as_str(),
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
        parse_identifier_no_space,
        spaces_or_comments,
    )(input)
    .map(|(input, name)| (input, name.to_string()))
}

fn parse_identifier_no_space(input: &str) -> IResult<&str, String> {
    recognize(pair(
        one_of("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ_"),
        many0(one_of(
            "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ_0123456789",
        )),
    ))(input)
    .map(|(input, name)| (input, name.to_string()))
}

// LitString ::=
//   | "'" ("\\'" | [^'])* "'"
//   | '"' ('\\"' | [^"])* '"'

pub fn parse_string_literal(input: &str) -> IResult<&str, String> {
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

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct MethodArgs(pub(super) Vec<LitExpr>);

// Comma-separated positional arguments for a method, surrounded by parentheses.
// When an arrow method is used without arguments, the Option<MethodArgs> for
// the PathSelection::Method will be None, so we can safely define MethodArgs
// using a Vec<LitExpr> in all cases (possibly empty but never missing).
impl MethodArgs {
    fn parse(input: &str) -> IResult<&str, Self> {
        delimited(
            tuple((spaces_or_comments, char('('), spaces_or_comments)),
            opt(map(
                tuple((
                    LitExpr::parse,
                    many0(preceded(char(','), LitExpr::parse)),
                    opt(char(',')),
                )),
                |(first, rest, _trailing_comma)| {
                    let mut output = vec![first];
                    output.extend(rest);
                    output
                },
            )),
            tuple((spaces_or_comments, char(')'), spaces_or_comments)),
        )(input)
        .map(|(input, args)| (input, Self(args.unwrap_or_default())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::selection;

    #[test]
    fn test_identifier() {
        assert_eq!(parse_identifier("hello"), Ok(("", "hello".to_string())));

        assert_eq!(
            parse_identifier("hello_world"),
            Ok(("", "hello_world".to_string())),
        );

        assert_eq!(
            parse_identifier("hello_world_123"),
            Ok(("", "hello_world_123".to_string())),
        );

        assert_eq!(parse_identifier(" hello "), Ok(("", "hello".to_string())));

        assert_eq!(
            parse_identifier_no_space("oyez"),
            Ok(("", "oyez".to_string())),
        );

        assert_eq!(
            parse_identifier_no_space("oyez   "),
            Ok(("   ", "oyez".to_string())),
        );

        assert_eq!(
            parse_identifier_no_space("  oyez   "),
            Err(nom::Err::Error(nom::error::Error::new(
                "  oyez   ",
                nom::error::ErrorKind::OneOf
            ))),
        );
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
                Alias::new("hello"),
            )),
        );

        assert_eq!(
            Alias::parse("hello :"),
            Ok((
                "",
                Alias::new("hello"),
            )),
        );

        assert_eq!(
            Alias::parse("hello : "),
            Ok((
                "",
                Alias::new("hello"),
            )),
        );

        assert_eq!(
            Alias::parse("  hello :"),
            Ok((
                "",
                Alias::new("hello"),
            )),
        );

        assert_eq!(
            Alias::parse("hello: "),
            Ok((
                "",
                Alias::new("hello"),
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
            NamedSelection::Field(None, Key::field("hello"), None),
            "hello",
        );

        assert_result_and_name(
            "hello { world }",
            NamedSelection::Field(
                None,
                Key::field("hello"),
                Some(SubSelection {
                    selections: vec![NamedSelection::Field(None, Key::field("world"), None)],
                    star: None,
                }),
            ),
            "hello",
        );

        assert_result_and_name(
            "hi: hello",
            NamedSelection::Field(
                Some(Alias::new("hi")),
                Key::field("hello"),
                None,
            ),
            "hi",
        );

        assert_result_and_name(
            "hi: 'hello world'",
            NamedSelection::Field(
                Some(Alias::new("hi")),
                Key::quoted("hello world"),
                None,
            ),
            "hi",
        );

        assert_result_and_name(
            "hi: hello { world }",
            NamedSelection::Field(
                Some(Alias::new("hi")),
                Key::field("hello"),
                Some(SubSelection {
                    selections: vec![NamedSelection::Field(None, Key::field("world"), None)],
                    star: None,
                }),
            ),
            "hi",
        );

        assert_result_and_name(
            "hey: hello { world again }",
            NamedSelection::Field(
                Some(Alias::new("hey")),
                Key::field("hello"),
                Some(SubSelection {
                    selections: vec![
                        NamedSelection::Field(None, Key::field("world"), None),
                        NamedSelection::Field(None, Key::field("again"), None),
                    ],
                    star: None,
                }),
            ),
            "hey",
        );

        assert_result_and_name(
            "hey: 'hello world' { again }",
            NamedSelection::Field(
                Some(Alias::new("hey")),
                Key::quoted("hello world"),
                Some(SubSelection {
                    selections: vec![NamedSelection::Field(None, Key::field("again"), None)],
                    star: None,
                }),
            ),
            "hey",
        );

        assert_result_and_name(
            "leggo: 'my ego'",
            NamedSelection::Field(
                Some(Alias::new("leggo")),
                Key::quoted("my ego"),
                None,
            ),
            "leggo",
        );

        assert_result_and_name(
            "'let go': 'my ego'",
            NamedSelection::Field(
                Some(Alias::quoted("let go")),
                Key::quoted("my ego"),
                None,
            ),
            "let go",
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
                selections: vec![NamedSelection::Field(None, Key::field("hello"), None),],
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
                    Alias::new("hi"),
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
                    NamedSelection::Field(None, Key::field("before"), None),
                    NamedSelection::Path(
                        Alias::new("hi"),
                        PathSelection::from_slice(
                            &[
                                Key::Field("hello".to_string()),
                                Key::Field("world".to_string()),
                            ],
                            None,
                        ),
                    ),
                    NamedSelection::Field(None, Key::field("after"), None),
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
                    NamedSelection::Field(None, Key::field("before"), None),
                    NamedSelection::Path(
                        Alias::new("hi"),
                        PathSelection::from_slice(
                            &[
                                Key::Field("hello".to_string()),
                                Key::Field("world".to_string()),
                            ],
                            Some(SubSelection {
                                selections: vec![
                                    NamedSelection::Field(None, Key::field("nested"), None),
                                    NamedSelection::Field(None, Key::field("names"), None),
                                ],
                                star: None,
                            }),
                        ),
                    ),
                    NamedSelection::Field(None, Key::field("after"), None),
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
                identifier: 'property name with spaces'
                'unaliased non-identifier property'
                'non-identifier alias': identifier

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
                    Some(Alias::new("topLevelAlias")),
                    Key::field("topLevelField"),
                    Some(SubSelection {
                        selections: vec![
                            NamedSelection::Field(
                                Some(Alias::new("identifier")),
                                Key::quoted("property name with spaces"),
                                None,
                            ),
                            NamedSelection::Field(
                                None,
                                Key::quoted("unaliased non-identifier property"),
                                None,
                            ),
                            NamedSelection::Field(
                                Some(Alias::quoted("non-identifier alias")),
                                Key::field("identifier"),
                                None,
                            ),
                            NamedSelection::Path(
                                Alias::new("pathSelection"),
                                PathSelection::from_slice(
                                    &[
                                        Key::Field("some".to_string()),
                                        Key::Field("nested".to_string()),
                                        Key::Field("path".to_string()),
                                    ],
                                    Some(SubSelection {
                                        selections: vec![
                                            NamedSelection::Field(
                                                Some(Alias::new("still")),
                                                Key::field("yet"),
                                                None,
                                            ),
                                            NamedSelection::Field(None, Key::field("more"), None,),
                                            NamedSelection::Field(
                                                None,
                                                Key::field("properties"),
                                                None,
                                            ),
                                        ],
                                        star: None,
                                    })
                                ),
                            ),
                            NamedSelection::Group(
                                Alias::new("siblingGroup"),
                                SubSelection {
                                    selections: vec![
                                        NamedSelection::Field(None, Key::field("brother"), None,),
                                        NamedSelection::Field(None, Key::field("sister"), None,),
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
                    selections: vec![NamedSelection::Field(None, Key::field("hello"), None)],
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
                    selections: vec![NamedSelection::Field(
                        Some(Alias::new("leggo")),
                        Key::quoted("my ego"),
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

        {
            let expected = PathSelection {
                path: PathList::Key(
                    Key::field("results"),
                    Box::new(PathList::Selection(SubSelection {
                        selections: vec![
                            NamedSelection::Field(None, Key::quoted("quoted without alias"), Some(SubSelection {
                                selections: vec![
                                    NamedSelection::Field(None, Key::field("id"), None),
                                    NamedSelection::Field(None, Key::quoted("n a m e"), None),
                                ],
                                star: None,
                            })),
                        ],
                        star: None,
                    })),
                )
            };
            check_path_selection(".results { 'quoted without alias' { id 'n a m e' } }", expected.clone());
            check_path_selection(".results{'quoted without alias'{id'n a m e'}}", expected.clone());
        }

        {
            let expected = PathSelection {
                path: PathList::Key(
                    Key::field("results"),
                    Box::new(PathList::Selection(SubSelection {
                        selections: vec![
                            NamedSelection::Field(
                                Some(Alias::quoted("non-identifier alias")),
                                Key::quoted("quoted with alias"),
                                Some(SubSelection {
                                    selections: vec![
                                        NamedSelection::Field(None, Key::field("id"), None),
                                        NamedSelection::Field(
                                            Some(Alias::quoted("n a m e")),
                                            Key::field("name"),
                                            None,
                                        ),
                                    ],
                                    star: None,
                                }),
                            ),
                        ],
                        star: None,
                    })),
                )
            };
            check_path_selection(
                ".results { 'non-identifier alias': 'quoted with alias' { id 'n a m e': name } }",
                expected.clone(),
            );
            check_path_selection(
                ".results{'non-identifier alias':'quoted with alias'{id'n a m e':name}}",
                expected.clone(),
            );
        }
    }

    #[test]
    fn test_path_selection_vars() {
        check_path_selection(
            "$this",
            PathList::Var(KnownVariable::This, Box::new(PathList::Empty)).into(),
        );

        check_path_selection(
            "$",
            PathList::Var(KnownVariable::Dollar, Box::new(PathList::Empty)).into(),
        );

        check_path_selection(
            "$this { hello }",
            PathList::Var(
                KnownVariable::This,
                Box::new(PathList::Selection(SubSelection {
                    selections: vec![NamedSelection::Field(None, Key::field("hello"), None)],
                    star: None,
                })),
            )
            .into(),
        );

        check_path_selection(
            "$ { hello }",
            PathList::Var(
                KnownVariable::Dollar,
                Box::new(PathList::Selection(SubSelection {
                    selections: vec![NamedSelection::Field(None, Key::field("hello"), None)],
                    star: None,
                })),
            )
            .into(),
        );

        check_path_selection(
            "$this { before alias: $args.arg after }",
            PathList::Var(
                KnownVariable::This,
                Box::new(PathList::Selection(SubSelection {
                    selections: vec![
                        NamedSelection::Field(None, Key::field("before"), None),
                        NamedSelection::Path(
                            Alias::new("alias"),
                            PathList::Var(
                                KnownVariable::Args,
                                Box::new(PathList::Key(
                                    Key::Field("arg".to_string()),
                                    Box::new(PathList::Empty),
                                )),
                            )
                            .into(),
                        ),
                        NamedSelection::Field(None, Key::field("after"), None),
                    ],
                    star: None,
                })),
            )
            .into(),
        );

        check_path_selection(
            "$.nested { key injected: $args.arg }",
            PathList::Var(
                KnownVariable::Dollar,
                Box::new(PathList::Key(
                    Key::Field("nested".to_string()),
                    Box::new(PathList::Selection(SubSelection {
                        selections: vec![
                            NamedSelection::Field(None, Key::field("key"), None),
                            NamedSelection::Path(
                                Alias::new("injected"),
                                PathList::Var(
                                    KnownVariable::Args,
                                    Box::new(PathList::Key(
                                        Key::Field("arg".to_string()),
                                        Box::new(PathList::Empty),
                                    )),
                                )
                                .into(),
                            ),
                        ],
                        star: None,
                    })),
                )),
            )
            .into(),
        );

        check_path_selection(
            "$args.a.b.c",
            PathList::Var(
                KnownVariable::Args,
                Box::new(PathList::from_slice(
                    &[
                        Key::Field("a".to_string()),
                        Key::Field("b".to_string()),
                        Key::Field("c".to_string()),
                    ],
                    None,
                )),
            )
            .into(),
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
            PathList::Var(
                KnownVariable::Dollar,
                Box::new(PathList::Key(
                    Key::Field("data".to_string()),
                    Box::new(PathList::Empty),
                )),
            )
            .into(),
        );

        check_path_selection(
            "$.data.'quoted property'.nested",
            PathList::Var(
                KnownVariable::Dollar,
                Box::new(PathList::Key(
                    Key::Field("data".to_string()),
                    Box::new(PathList::Key(
                        Key::Quoted("quoted property".to_string()),
                        Box::new(PathList::Key(
                            Key::Field("nested".to_string()),
                            Box::new(PathList::Empty),
                        )),
                    )),
                )),
            )
            .into(),
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
            JSONSelection::Path(
                PathList::Var(KnownVariable::Dollar, Box::new(PathList::Empty)).into()
            ),
        );

        assert_eq!(
            selection!("$this"),
            JSONSelection::Path(
                PathList::Var(KnownVariable::This, Box::new(PathList::Empty)).into()
            ),
        );

        assert_eq!(
            selection!("value: $ a { b c }"),
            JSONSelection::Named(SubSelection {
                selections: vec![
                    NamedSelection::Path(
                        Alias::new("value"),
                        PathSelection {
                            path: PathList::Var(KnownVariable::Dollar, Box::new(PathList::Empty)),
                        },
                    ),
                    NamedSelection::Field(
                        None,
                        Key::field("a"),
                        Some(SubSelection {
                            selections: vec![
                                NamedSelection::Field(None, Key::field("b"), None),
                                NamedSelection::Field(None, Key::field("c"), None),
                            ],
                            star: None,
                        }),
                    ),
                ],
                star: None,
            }),
        );
        assert_eq!(
            selection!("value: $this { b c }"),
            JSONSelection::Named(SubSelection {
                selections: vec![NamedSelection::Path(
                    Alias::new("value"),
                    PathSelection {
                        path: PathList::Var(
                            KnownVariable::This,
                            Box::new(PathList::Selection(SubSelection {
                                selections: vec![
                                    NamedSelection::Field(None, Key::field("b"), None),
                                    NamedSelection::Field(None, Key::field("c"), None),
                                ],
                                star: None,
                            })),
                        ),
                    },
                ),],
                star: None,
            }),
        );
    }

    #[test]
    fn test_path_selection_at() {
        check_path_selection(
            "@",
            PathSelection {
                path: PathList::Var(KnownVariable::AtSign, Box::new(PathList::Empty)),
            },
        );

        check_path_selection(
            "@.a.b.c",
            PathSelection {
                path: PathList::Var(
                    KnownVariable::AtSign,
                    Box::new(PathList::from_slice(
                        &[
                            Key::Field("a".to_string()),
                            Key::Field("b".to_string()),
                            Key::Field("c".to_string()),
                        ],
                        None,
                    )),
                ),
            },
        );

        check_path_selection(
            "@.items->first",
            PathSelection {
                path: PathList::Var(
                    KnownVariable::AtSign,
                    Box::new(PathList::Key(
                        Key::Field("items".to_string()),
                        Box::new(PathList::Method(
                            "first".to_string(),
                            None,
                            Box::new(PathList::Empty),
                        )),
                    )),
                ),
            },
        );
    }

    #[test]
    fn test_path_methods() {
        check_path_selection(
            "data.x->or(data.y)",
            PathSelection {
                path: PathList::Key(
                    Key::Field("data".to_string()),
                    Box::new(PathList::Key(
                        Key::Field("x".to_string()),
                        Box::new(PathList::Method(
                            "or".to_string(),
                            Some(MethodArgs(vec![LitExpr::Path(PathSelection::from_slice(
                                &[Key::Field("data".to_string()), Key::Field("y".to_string())],
                                None,
                            ))])),
                            Box::new(PathList::Empty),
                        )),
                    )),
                ),
            },
        );

        {
            let expected = PathSelection {
                path: PathList::Key(
                    Key::Field("data".to_string()),
                    Box::new(PathList::Method(
                        "query".to_string(),
                        Some(MethodArgs(vec![
                            LitExpr::Path(PathSelection::from_slice(
                                &[Key::Field("a".to_string())],
                                None,
                            )),
                            LitExpr::Path(PathSelection::from_slice(
                                &[Key::Field("b".to_string())],
                                None,
                            )),
                            LitExpr::Path(PathSelection::from_slice(
                                &[Key::Field("c".to_string())],
                                None,
                            )),
                        ])),
                        Box::new(PathList::Empty),
                    )),
                ),
            };
            check_path_selection("data->query(.a, .b, .c)", expected.clone());
            check_path_selection("data->query(.a, .b, .c )", expected.clone());
            check_path_selection("data->query(.a, .b, .c,)", expected.clone());
            check_path_selection("data->query(.a, .b, .c ,)", expected.clone());
            check_path_selection("data->query(.a, .b, .c , )", expected.clone());
        }

        {
            let expected = PathSelection {
                path: PathList::Key(
                    Key::Field("data".to_string()),
                    Box::new(PathList::Key(
                        Key::Field("x".to_string()),
                        Box::new(PathList::Method(
                            "concat".to_string(),
                            Some(MethodArgs(vec![LitExpr::Array(vec![
                                LitExpr::Path(PathSelection::from_slice(
                                    &[Key::Field("data".to_string()), Key::Field("y".to_string())],
                                    None,
                                )),
                                LitExpr::Path(PathSelection::from_slice(
                                    &[Key::Field("data".to_string()), Key::Field("z".to_string())],
                                    None,
                                )),
                            ])])),
                            Box::new(PathList::Empty),
                        )),
                    )),
                ),
            };
            check_path_selection("data.x->concat([data.y, data.z])", expected.clone());
            check_path_selection("data.x->concat([ data.y, data.z ])", expected.clone());
            check_path_selection("data.x->concat([data.y, data.z,])", expected.clone());
            check_path_selection("data.x->concat([data.y, data.z , ])", expected.clone());
            check_path_selection("data.x->concat([data.y, data.z,],)", expected.clone());
            check_path_selection("data.x->concat([data.y, data.z , ] , )", expected.clone());
        }

        check_path_selection(
            "data->method([$ { x2: x->times(2) }, $ { y2: y->times(2) }])",
            PathSelection {
                path: PathList::Key(
                    Key::Field("data".to_string()),
                    Box::new(PathList::Method(
                        "method".to_string(),
                        Some(MethodArgs(vec![LitExpr::Array(vec![
                            LitExpr::Path(PathSelection {
                                path: PathList::Var(
                                    KnownVariable::Dollar,
                                    Box::new(PathList::Selection(SubSelection {
                                        selections: vec![NamedSelection::Path(
                                            Alias::new("x2"),
                                            PathSelection {
                                                path: PathList::Key(
                                                    Key::Field("x".to_string()),
                                                    Box::new(PathList::Method(
                                                        "times".to_string(),
                                                        Some(MethodArgs(vec![LitExpr::Number(
                                                            "2".parse().expect(
                                                                "serde_json::Number parse error",
                                                            ),
                                                        )])),
                                                        Box::new(PathList::Empty),
                                                    )),
                                                ),
                                            },
                                        )],
                                        star: None,
                                    })),
                                ),
                            }),
                            LitExpr::Path(PathSelection {
                                path: PathList::Var(
                                    KnownVariable::Dollar,
                                    Box::new(PathList::Selection(SubSelection {
                                        selections: vec![NamedSelection::Path(
                                            Alias::new("y2"),
                                            PathSelection {
                                                path: PathList::Key(
                                                    Key::Field("y".to_string()),
                                                    Box::new(PathList::Method(
                                                        "times".to_string(),
                                                        Some(MethodArgs(vec![LitExpr::Number(
                                                            "2".parse().expect(
                                                                "serde_json::Number parse error",
                                                            ),
                                                        )])),
                                                        Box::new(PathList::Empty),
                                                    )),
                                                ),
                                            },
                                        )],
                                        star: None,
                                    })),
                                ),
                            }),
                        ])])),
                        Box::new(PathList::Empty),
                    )),
                ),
            },
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
                    selections: vec![NamedSelection::Field(None, Key::field("hello"), None)],
                    star: None,
                },
            )),
        );

        assert_eq!(
            SubSelection::parse("{ hello }"),
            Ok((
                "",
                SubSelection {
                    selections: vec![NamedSelection::Field(None, Key::field("hello"), None)],
                    star: None,
                },
            )),
        );

        assert_eq!(
            SubSelection::parse("  { padded  } "),
            Ok((
                "",
                SubSelection {
                    selections: vec![NamedSelection::Field(None, Key::field("padded"), None)],
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
                        NamedSelection::Field(None, Key::field("hello"), None),
                        NamedSelection::Field(None, Key::field("world"), None),
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
                        Key::field("hello"),
                        Some(SubSelection {
                            selections: vec![NamedSelection::Field(
                                None,
                                Key::field("world"),
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
                    Some(Alias::new("rest")),
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
                        selections: vec![NamedSelection::Field(None, Key::field("hello"), None),],
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
                    Some(Alias::new("hi")),
                    Some(Box::new(SubSelection {
                        selections: vec![NamedSelection::Field(None, Key::field("hello"), None),],
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
                    Some(Alias::new("alias")),
                    Some(Box::new(SubSelection {
                        selections: vec![
                            NamedSelection::Field(None, Key::field("x"), None),
                            NamedSelection::Field(None, Key::field("y"), None),
                            NamedSelection::Field(None, Key::field("z"), None),
                        ],
                        star: Some(StarSelection(
                            Some(Alias::new("rest")),
                            None
                        )),
                    })),
                ),
            )),
        );

        assert_eq!(
            selection!(" before alias: * { * { a b c } } "),
            JSONSelection::Named(SubSelection {
                selections: vec![NamedSelection::Field(None, Key::field("before"), None),],
                star: Some(StarSelection(
                    Some(Alias::new("alias")),
                    Some(Box::new(SubSelection {
                        selections: vec![],
                        star: Some(StarSelection(
                            None,
                            Some(Box::new(SubSelection {
                                selections: vec![
                                    NamedSelection::Field(None, Key::field("a"), None),
                                    NamedSelection::Field(None, Key::field("b"), None),
                                    NamedSelection::Field(None, Key::field("c"), None),
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
                    NamedSelection::Field(None, Key::field("before"), None),
                    NamedSelection::Group(
                        Alias::new("group"),
                        SubSelection {
                            selections: vec![],
                            star: Some(StarSelection(
                                None,
                                Some(Box::new(SubSelection {
                                    selections: vec![
                                        NamedSelection::Field(None, Key::field("a"), None),
                                        NamedSelection::Field(None, Key::field("b"), None),
                                        NamedSelection::Field(None, Key::field("c"), None),
                                    ],
                                    star: None,
                                }))
                            )),
                        },
                    ),
                    NamedSelection::Field(None, Key::field("after"), None),
                ],
                star: None,
            }),
        );
    }

    #[test]
    fn test_external_var_paths() {
        {
            let sel = selection!(
                r#"
                $->echo([$args.arg1, $args.arg2, @.items->first])
            "#
            );
            let args_arg1_path = PathSelection::parse("$args.arg1").unwrap().1;
            let args_arg2_path = PathSelection::parse("$args.arg2").unwrap().1;
            assert_eq!(
                sel.external_var_paths(),
                vec![&args_arg1_path, &args_arg2_path,]
            );
        }
        {
            let sel = selection!(
                r#"
                $this.kind->match(
                    ["A", $this.a],
                    ["B", $this.b],
                    ["C", $this.c],
                    [@, @->to_lower_case],
                )
            "#
            );
            let this_kind_path = match &sel {
                JSONSelection::Path(path) => path,
                _ => panic!("Expected PathSelection"),
            };
            let this_a_path = PathSelection::parse("$this.a").unwrap().1;
            let this_b_path = PathSelection::parse("$this.b").unwrap().1;
            let this_c_path = PathSelection::parse("$this.c").unwrap().1;
            assert_eq!(
                sel.external_var_paths(),
                vec![this_kind_path, &this_a_path, &this_b_path, &this_c_path,]
            );
        }
        {
            let sel = selection!(
                r#"
                data.results->slice($args.start, $args.end) {
                    id
                    __typename: $args.type
                }
            "#
            );
            let start_path = PathSelection::parse("$args.start").unwrap().1;
            let end_path = PathSelection::parse("$args.end").unwrap().1;
            let args_type_path = PathSelection::parse("$args.type").unwrap().1;
            assert_eq!(
                sel.external_var_paths(),
                vec![&start_path, &end_path, &args_type_path]
            );
        }
    }
}
