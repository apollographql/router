use std::fmt::Display;

use nom::branch::alt;
use nom::character::complete::char;
use nom::character::complete::one_of;
use nom::combinator::all_consuming;
use nom::combinator::map;
use nom::combinator::opt;
use nom::combinator::recognize;
use nom::multi::many0;
use nom::sequence::pair;
use nom::sequence::preceded;
use nom::sequence::terminated;
use nom::sequence::tuple;
use nom::IResult;
use nom::Slice;
use serde_json_bytes::Value as JSON;

use super::helpers::spaces_or_comments;
use super::known_var::KnownVariable;
use super::lit_expr::LitExpr;
use super::location::merge_ranges;
use super::location::ranged_span;
use super::location::OffsetRange;
use super::location::Ranged;
use super::location::Span;
use super::location::WithRange;

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
            ..Default::default()
        })
    }

    pub fn is_empty(&self) -> bool {
        match self {
            JSONSelection::Named(subselect) => {
                subselect.selections.is_empty() && subselect.star.is_none()
            }
            JSONSelection::Path(path) => *path.path == PathList::Empty,
        }
    }

    pub fn parse(input: &str) -> IResult<&str, Self> {
        let input_span = Span::new(input);

        match alt((
            all_consuming(terminated(
                map(SubSelection::parse_naked, Self::Named),
                // By convention, most ::parse methods do not consume trailing
                // spaces_or_comments, so we need to consume them here in order
                // to satisfy the all_consuming requirement.
                spaces_or_comments,
            )),
            all_consuming(terminated(
                map(PathSelection::parse, Self::Path),
                // It's tempting to hoist the all_consuming(terminated(...))
                // checks outside the alt((...)) so we only need to handle
                // trailing spaces_or_comments once, but that won't work because
                // the Self::Named case should fail when parsing a single
                // PathSelection, and that failure typically happens because the
                // SubSelection::parse_naked method does not consume the entire
                // input, which is caught by the first all_consuming above.
                spaces_or_comments,
            )),
        ))(input_span)
        {
            Ok((remainder, selection)) => {
                if remainder.fragment().is_empty() {
                    Ok(("", selection))
                } else {
                    Err(nom::Err::Error(nom::error::Error::new(
                        *input_span.fragment(),
                        nom::error::ErrorKind::IsNot,
                    )))
                }
            }
            Err(_) => Err(nom::Err::Error(nom::error::Error::new(
                input,
                nom::error::ErrorKind::IsNot,
            ))),
        }
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
    Field(Option<Alias>, WithRange<Key>, Option<SubSelection>),
    Path(Alias, PathSelection),
    Group(Alias, SubSelection),
}

// Like PathSelection, NamedSelection is an AST structure that takes its range
// entirely from its children, so NamedSelection itself does not need to provide
// separate storage for its own range, and therefore does not need to be wrapped
// as WithRange<NamedSelection>, but merely needs to implement the Ranged trait.
impl Ranged<NamedSelection> for NamedSelection {
    fn node(&self) -> &NamedSelection {
        self
    }

    fn range(&self) -> OffsetRange {
        match self {
            Self::Field(alias, key, sub) => {
                let range = key.range();
                let range = if let Some(alias) = alias.as_ref() {
                    merge_ranges(alias.range(), range)
                } else {
                    range
                };
                if let Some(sub) = sub.as_ref() {
                    merge_ranges(range, sub.range())
                } else {
                    range
                }
            }
            Self::Path(alias, path) => merge_ranges(alias.range(), path.range()),
            Self::Group(alias, sub) => merge_ranges(alias.range(), sub.range()),
        }
    }
}

impl NamedSelection {
    pub(crate) fn parse(input: Span) -> IResult<Span, Self> {
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

    fn parse_field(input: Span) -> IResult<Span, Self> {
        tuple((
            opt(Alias::parse),
            Key::parse,
            spaces_or_comments,
            opt(SubSelection::parse),
        ))(input)
        .map(|(remainder, (alias, name, _, selection))| {
            (remainder, Self::Field(alias, name, selection))
        })
    }

    fn parse_path(input: Span) -> IResult<Span, Self> {
        tuple((Alias::parse, PathSelection::parse))(input)
            .map(|(input, (alias, path))| (input, Self::Path(alias, path)))
    }

    fn parse_group(input: Span) -> IResult<Span, Self> {
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
    pub(super) path: WithRange<PathList>,
}

// Like NamedSelection, PathSelection is an AST structure that takes its range
// entirely from self.path (a WithRange<PathList>), so PathSelection itself does
// not need to be wrapped as WithRange<PathSelection>, but merely needs to
// implement the Ranged trait.
impl Ranged<PathSelection> for PathSelection {
    fn node(&self) -> &PathSelection {
        self
    }

    fn range(&self) -> OffsetRange {
        self.path.range()
    }
}

impl PathSelection {
    pub fn parse(input: Span) -> IResult<Span, Self> {
        PathList::parse(input).map(|(input, path)| (input, Self { path }))
    }

    pub(crate) fn var_name_and_nested_keys(&self) -> Option<(&KnownVariable, Vec<&str>)> {
        match self.path.as_ref() {
            PathList::Var(var_name, tail) => Some((var_name, tail.prefix_of_keys())),
            _ => None,
        }
    }

    pub(super) fn is_single_key(&self) -> bool {
        self.path.is_single_key()
    }

    pub(super) fn from_slice(keys: &[Key], selection: Option<SubSelection>) -> Self {
        Self {
            path: WithRange::new(PathList::from_slice(keys, selection), None),
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
        match self.path.node() {
            PathList::Var(var_name, tail) => {
                // The $ and @ variables refer to parts of the current JSON
                // data, so they do not need to be surfaced as external variable
                // references.
                if var_name != &KnownVariable::Dollar && var_name != &KnownVariable::AtSign {
                    paths.push(self);
                }
                paths.extend(tail.external_var_paths());
            }
            other => {
                paths.extend(other.external_var_paths());
            }
        };
        paths
    }
}

impl From<PathList> for PathSelection {
    fn from(path: PathList) -> Self {
        Self {
            path: WithRange::new(path, None),
        }
    }
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub(super) enum PathList {
    // A VarPath must start with a variable (either $identifier, $, or @),
    // followed by any number of PathStep items (the WithRange<PathList>).
    // Because we represent the @ quasi-variable using PathList::Var, this
    // variant handles both VarPath and AtPath from the grammar. The String
    // variable name must always contain the $ character. The PathList::Var
    // variant may only appear at the beginning of a PathSelection's PathList,
    // not in the middle.
    Var(WithRange<KnownVariable>, WithRange<PathList>),

    // A PathSelection that starts with a PathList::Key is a KeyPath, but a
    // PathList::Key also counts as PathStep item, so it may also appear in the
    // middle/tail of a PathList.
    Key(WithRange<Key>, WithRange<PathList>),

    // A PathList::Method is a PathStep item that may appear only in the
    // middle/tail (not the beginning) of a PathSelection. Methods are
    // distinguished from .keys by their ->method invocation syntax.
    Method(WithRange<String>, Option<MethodArgs>, WithRange<PathList>),

    // Optionally, a PathList may end with a SubSelection, which applies a set
    // of named selections to the final value of the path. PathList::Selection
    // by itself is not a valid PathList.
    Selection(SubSelection),

    // Every PathList must be terminated by either PathList::Selection or
    // PathList::Empty. PathList::Empty by itself is not a valid PathList.
    Empty,
}

impl PathList {
    pub fn parse(input: Span) -> IResult<Span, WithRange<Self>> {
        match Self::parse_with_depth(input, 0) {
            Ok((remainder, parsed)) if matches!(*parsed, Self::Empty) => Err(nom::Err::Error(
                nom::error::Error::new(remainder, nom::error::ErrorKind::IsNot),
            )),
            otherwise => otherwise,
        }
    }

    pub(super) fn into_with_range(self) -> WithRange<Self> {
        WithRange::new(self, None)
    }

    fn parse_with_depth(input: Span, depth: usize) -> IResult<Span, WithRange<Self>> {
        // If the input is empty (i.e. this method will end up returning
        // PathList::Empty), we want the OffsetRange to be an empty range at the
        // end of the previously parsed PathList elements, not separated from
        // them by trailing spaces or comments, so we need to capture the empty
        // range before consuming leading spaces_or_comments.
        let offset_if_empty = input.location_offset();
        let range_if_empty: OffsetRange = Some(offset_if_empty..offset_if_empty);

        // Consume leading spaces_or_comments for all cases below.
        let (input, _spaces) = spaces_or_comments(input)?;

        // Variable references (including @ references) and key references
        // without a leading . are accepted only at depth 0, or at the beginning
        // of the PathSelection.
        if depth == 0 {
            if let Ok((suffix, (dollar, opt_var))) =
                tuple((ranged_span("$"), opt(parse_identifier_no_space)))(input)
            {
                let dollar_range = dollar.range();
                let (remainder, rest) = Self::parse_with_depth(suffix, depth + 1)?;
                let full_range = merge_ranges(dollar_range.clone(), rest.range());
                return if let Some(var) = opt_var {
                    let full_name = format!("{}{}", dollar.node(), var.as_str());
                    if let Some(known_var) = KnownVariable::from_str(full_name.as_str()) {
                        let var_range = merge_ranges(dollar_range.clone(), var.range());
                        let ranged_known_var = WithRange::new(known_var, var_range);
                        Ok((
                            remainder,
                            WithRange::new(Self::Var(ranged_known_var, rest), full_range),
                        ))
                    } else {
                        // Reject unknown variables at parse time.
                        // TODO Improve these parse error messages.
                        Err(nom::Err::Error(nom::error::Error::new(
                            input,
                            nom::error::ErrorKind::IsNot,
                        )))
                    }
                } else {
                    let ranged_dollar_var =
                        WithRange::new(KnownVariable::Dollar, dollar_range.clone());
                    Ok((
                        remainder,
                        WithRange::new(Self::Var(ranged_dollar_var, rest), full_range),
                    ))
                };
            }

            if let Ok((suffix, at)) = ranged_span("@")(input) {
                let (remainder, rest) = Self::parse_with_depth(suffix, depth + 1)?;
                let full_range = merge_ranges(at.range(), rest.range());
                // Because we include the $ in the variable name for ordinary
                // variables, we have the freedom to store other symbols as
                // special variables, such as @ for the current value. In fact,
                // as long as we can parse the token(s) as a PathList::Var, the
                // name of a variable could technically be any string we like.
                return Ok((
                    remainder,
                    WithRange::new(
                        Self::Var(WithRange::new(KnownVariable::AtSign, at.range()), rest),
                        full_range,
                    ),
                ));
            }

            if let Ok((suffix, key)) = Key::parse(input) {
                let (remainder, rest) = Self::parse_with_depth(suffix, depth + 1)?;
                return match rest.node() {
                    Self::Empty | Self::Selection(_) => Err(nom::Err::Error(
                        nom::error::Error::new(remainder, nom::error::ErrorKind::IsNot),
                    )),
                    _ => {
                        let full_range = merge_ranges(key.range(), rest.range());
                        Ok((remainder, WithRange::new(Self::Key(key, rest), full_range)))
                    }
                };
            }
        }

        // The .key case is applicable at any depth. If it comes first in the
        // path selection, $.key is implied, but the distinction is preserved
        // (using Self::Path rather than Self::Var) for accurate reprintability.
        if let Ok((suffix, (dot, _, key))) =
            tuple((ranged_span("."), spaces_or_comments, Key::parse))(input)
        {
            let (remainder, rest) = Self::parse_with_depth(suffix, depth + 1)?;
            let dot_key_range = merge_ranges(dot.range(), key.range());
            let full_range = merge_ranges(dot_key_range, rest.range());
            return Ok((remainder, WithRange::new(Self::Key(key, rest), full_range)));
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
        if let Ok((suffix, (arrow, _, method, args))) = tuple((
            ranged_span("->"),
            spaces_or_comments,
            parse_identifier,
            opt(MethodArgs::parse),
        ))(input)
        {
            let (remainder, rest) = Self::parse_with_depth(suffix, depth + 1)?;
            let full_range = merge_ranges(arrow.range(), rest.range());
            return Ok((
                remainder,
                WithRange::new(Self::Method(method, args, rest), full_range),
            ));
        }

        // Likewise, if the PathSelection has a SubSelection, it must appear at
        // the end of a non-empty path.
        if let Ok((suffix, selection)) = SubSelection::parse(input) {
            let selection_range = selection.range();
            return Ok((
                suffix,
                WithRange::new(Self::Selection(selection), selection_range),
            ));
        }

        // The Self::Empty enum case is used to indicate the end of a
        // PathSelection that has no SubSelection.
        Ok((input, WithRange::new(Self::Empty, range_if_empty)))
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
            [head, tail @ ..] => Self::Key(
                WithRange::new(head.clone(), None),
                WithRange::new(Self::from_slice(tail, selection), None),
            ),
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
                    for lit_arg in &args.args {
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
    pub(super) range: OffsetRange,
}

impl Ranged<SubSelection> for SubSelection {
    fn node(&self) -> &SubSelection {
        self
    }

    // Since SubSelection is a struct, we can store its range directly as a
    // field of the struct, allowing SubSelection to implement the Ranged trait
    // without a WithRange<SubSelection> wrapper.
    fn range(&self) -> OffsetRange {
        self.range.clone()
    }
}

impl SubSelection {
    pub(crate) fn parse(input: Span) -> IResult<Span, Self> {
        tuple((
            spaces_or_comments,
            ranged_span("{"),
            Self::parse_naked,
            spaces_or_comments,
            ranged_span("}"),
        ))(input)
        .map(|(remainder, (_, open_brace, sub, _, close_brace))| {
            let range = merge_ranges(open_brace.range(), close_brace.range());
            (
                remainder,
                Self {
                    selections: sub.selections,
                    star: sub.star,
                    range,
                },
            )
        })
    }

    fn parse_naked(input: Span) -> IResult<Span, Self> {
        tuple((
            spaces_or_comments,
            many0(NamedSelection::parse),
            // Note that when a * selection is used, it must be the last
            // selection in the SubSelection, since it does not count as a
            // NamedSelection, and is stored as a separate field from the
            // selections vector.
            opt(StarSelection::parse),
        ))(input)
        .map(|(remainder, (_, selections, star))| {
            let range = merge_ranges(
                selections.first().and_then(|first| first.range()),
                selections.last().and_then(|last| last.range()),
            );
            let range = if let Some(star) = star.as_ref() {
                merge_ranges(range, star.range())
            } else {
                range
            };
            (
                remainder,
                Self {
                    selections,
                    star,
                    range,
                },
            )
        })
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

#[derive(Debug, PartialEq, Eq, Clone, Default)]
pub struct StarSelection {
    pub(super) alias: Option<Alias>,
    pub(super) selection: Option<Box<SubSelection>>,
    pub(super) range: OffsetRange,
}

impl Ranged<StarSelection> for StarSelection {
    fn node(&self) -> &StarSelection {
        self
    }

    fn range(&self) -> OffsetRange {
        self.range.clone()
    }
}

impl StarSelection {
    pub(crate) fn new(alias: Option<Alias>, selection: Option<SubSelection>) -> Self {
        Self {
            alias,
            selection: selection.map(Box::new),
            range: None,
        }
    }

    pub(crate) fn parse(input: Span) -> IResult<Span, Self> {
        tuple((
            // The spaces_or_comments separators are necessary here because
            // Alias::parse and SubSelection::parse only consume surrounding
            // spaces when they match, and they are both optional here.
            opt(Alias::parse),
            spaces_or_comments,
            ranged_span("*"),
            opt(SubSelection::parse),
        ))(input)
        .map(|(remainder, (alias, _, star, selection))| {
            let range = star.range();
            let range = if let Some(alias) = alias.as_ref() {
                merge_ranges(alias.range(), range)
            } else {
                range
            };
            let range = if let Some(selection) = selection.as_ref() {
                merge_ranges(range, selection.range())
            } else {
                range
            };
            (
                remainder,
                Self {
                    alias,
                    selection: selection.map(Box::new),
                    range,
                },
            )
        })
    }
}

// Alias ::= Key ":"

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct Alias {
    pub(super) name: WithRange<Key>,
    pub(super) range: OffsetRange,
}

impl Ranged<Alias> for Alias {
    fn node(&self) -> &Alias {
        self
    }

    fn range(&self) -> OffsetRange {
        self.range.clone()
    }
}

impl Alias {
    pub fn new(name: &str) -> Self {
        Self {
            name: WithRange::new(Key::field(name), None),
            range: None,
        }
    }

    pub fn quoted(name: &str) -> Self {
        Self {
            name: WithRange::new(Key::quoted(name), None),
            range: None,
        }
    }

    fn parse(input: Span) -> IResult<Span, Self> {
        tuple((Key::parse, spaces_or_comments, ranged_span(":")))(input).map(
            |(input, (name, _, colon))| {
                let range = merge_ranges(name.range(), colon.range());
                (input, Self { name, range })
            },
        )
    }

    pub fn name(&self) -> &str {
        self.name.as_str()
    }
}

// Key ::= Identifier | LitString

#[derive(Debug, PartialEq, Eq, Clone, Hash)]
pub enum Key {
    Field(String),
    Quoted(String),
}

impl Key {
    pub fn parse(input: Span) -> IResult<Span, WithRange<Self>> {
        alt((
            map(parse_identifier, |id| id.take_as(Key::Field)),
            map(parse_string_literal, |s| s.take_as(Key::Quoted)),
        ))(input)
    }

    pub fn field(name: &str) -> Self {
        Self::Field(name.to_string())
    }

    pub fn quoted(name: &str) -> Self {
        Self::Quoted(name.to_string())
    }

    pub fn into_with_range(self) -> WithRange<Self> {
        WithRange::new(self, None)
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

fn parse_identifier(input: Span) -> IResult<Span, WithRange<String>> {
    preceded(spaces_or_comments, parse_identifier_no_space)(input)
}

fn parse_identifier_no_space(input: Span) -> IResult<Span, WithRange<String>> {
    recognize(pair(
        one_of("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ_"),
        many0(one_of(
            "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ_0123456789",
        )),
    ))(input)
    .map(|(remainder, name)| {
        let range = Some(name.location_offset()..remainder.location_offset());
        (remainder, WithRange::new(name.to_string(), range))
    })
}

// LitString ::=
//   | "'" ("\\'" | [^'])* "'"
//   | '"' ('\\"' | [^"])* '"'

pub fn parse_string_literal(input: Span) -> IResult<Span, WithRange<String>> {
    let input = spaces_or_comments(input)?.0;
    let start = input.location_offset();
    let mut input_char_indices = input.char_indices();

    match input_char_indices.next() {
        Some((0, quote @ '\'')) | Some((0, quote @ '"')) => {
            let mut escape_next = false;
            let mut chars: Vec<char> = vec![];
            let mut remainder: Option<Span> = None;

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
                    remainder = Some(input.slice(i + 1..));
                    break;
                }
                chars.push(c);
            }

            if let Some(remainder) = remainder {
                Ok((
                    remainder,
                    WithRange::new(
                        chars.iter().collect::<String>(),
                        Some(start..remainder.location_offset()),
                    ),
                ))
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

#[derive(Debug, PartialEq, Eq, Clone, Default)]
pub struct MethodArgs {
    pub(super) args: Vec<WithRange<LitExpr>>,
    pub(super) range: OffsetRange,
}

impl Ranged<MethodArgs> for MethodArgs {
    fn node(&self) -> &MethodArgs {
        self
    }

    fn range(&self) -> OffsetRange {
        self.range.clone()
    }
}

// Comma-separated positional arguments for a method, surrounded by parentheses.
// When an arrow method is used without arguments, the Option<MethodArgs> for
// the PathSelection::Method will be None, so we can safely define MethodArgs
// using a Vec<LitExpr> in all cases (possibly empty but never missing).
impl MethodArgs {
    fn parse(input: Span) -> IResult<Span, Self> {
        tuple((
            spaces_or_comments,
            ranged_span("("),
            spaces_or_comments,
            opt(map(
                tuple((
                    LitExpr::parse,
                    many0(preceded(
                        tuple((spaces_or_comments, char(','))),
                        LitExpr::parse,
                    )),
                    opt(tuple((spaces_or_comments, char(',')))),
                )),
                |(first, rest, _trailing_comma)| {
                    let mut output = vec![first];
                    output.extend(rest);
                    output
                },
            )),
            spaces_or_comments,
            ranged_span(")"),
        ))(input)
        .map(|(remainder, (_, open_paren, _, args, _, close_paren))| {
            let range = merge_ranges(open_paren.range(), close_paren.range());
            (
                remainder,
                Self {
                    args: args.unwrap_or_default(),
                    range,
                },
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::location::strip_ranges::StripRanges;
    use super::*;
    use crate::selection;
    use crate::sources::connect::json_selection::helpers::span_is_all_spaces_or_comments;

    #[test]
    fn test_identifier() {
        fn check(input: &str, expected_name: &str) {
            let (remainder, name) = parse_identifier(Span::new(input)).unwrap();
            assert!(span_is_all_spaces_or_comments(remainder));
            assert_eq!(name.node(), expected_name);
        }

        check("hello", "hello");
        check("hello_world", "hello_world");
        check("  hello_world ", "hello_world");
        check("hello_world_123", "hello_world_123");
        check(" hello ", "hello");

        fn check_no_space(input: &str, expected_name: &str) {
            let name = parse_identifier_no_space(Span::new(input)).unwrap().1;
            assert_eq!(name.node(), expected_name);
        }

        check_no_space("oyez", "oyez");
        check_no_space("oyez   ", "oyez");

        assert_eq!(
            parse_identifier_no_space(Span::new("  oyez   ")),
            Err(nom::Err::Error(nom::error::Error::new(
                Span::new("  oyez   "),
                nom::error::ErrorKind::OneOf
            ))),
        );
    }

    #[test]
    fn test_string_literal() {
        fn check(input: &str, expected: &str) {
            let (remainder, lit) = parse_string_literal(Span::new(input)).unwrap();
            assert!(span_is_all_spaces_or_comments(remainder));
            assert_eq!(lit.node(), expected);
        }
        check("'hello world'", "hello world");
        check("\"hello world\"", "hello world");
        check("'hello \"world\"'", "hello \"world\"");
        check("\"hello \\\"world\\\"\"", "hello \"world\"");
        check("'hello \\'world\\''", "hello 'world'");
    }

    #[test]
    fn test_key() {
        fn check(input: &str, expected: &Key) {
            let (remainder, key) = Key::parse(Span::new(input)).unwrap();
            assert!(span_is_all_spaces_or_comments(remainder));
            assert_eq!(key.node(), expected);
        }

        check("hello", &Key::field("hello"));
        check("'hello'", &Key::quoted("hello"));
        check("  hello ", &Key::field("hello"));
        check("\"hello\"", &Key::quoted("hello"));
        check("  \"hello\" ", &Key::quoted("hello"));
    }

    #[test]
    fn test_alias() {
        fn check(input: &str, alias: &str) {
            let (remainder, parsed) = Alias::parse(Span::new(input)).unwrap();
            assert!(span_is_all_spaces_or_comments(remainder));
            assert_eq!(parsed.name(), alias);
        }

        check("hello:", "hello");
        check("hello :", "hello");
        check("hello : ", "hello");
        check("  hello :", "hello");
        check("hello: ", "hello");
    }

    #[test]
    fn test_named_selection() {
        fn assert_result_and_name(input: &str, expected: NamedSelection, name: &str) {
            let (remainder, selection) = NamedSelection::parse(Span::new(input)).unwrap();
            assert!(span_is_all_spaces_or_comments(remainder));
            let selection = selection.strip_ranges();
            assert_eq!(selection.node(), &expected);
            assert_eq!(selection.node().name(), name);
            assert_eq!(
                selection!(input).strip_ranges(),
                JSONSelection::Named(SubSelection {
                    selections: vec![expected],
                    ..Default::default()
                },),
            );
        }

        assert_result_and_name(
            "hello",
            NamedSelection::Field(None, Key::field("hello").into_with_range(), None),
            "hello",
        );

        assert_result_and_name(
            "hello { world }",
            NamedSelection::Field(
                None,
                Key::field("hello").into_with_range(),
                Some(SubSelection {
                    selections: vec![NamedSelection::Field(
                        None,
                        Key::field("world").into_with_range(),
                        None,
                    )],
                    ..Default::default()
                }),
            ),
            "hello",
        );

        assert_result_and_name(
            "hi: hello",
            NamedSelection::Field(
                Some(Alias::new("hi")),
                Key::field("hello").into_with_range(),
                None,
            ),
            "hi",
        );

        assert_result_and_name(
            "hi: 'hello world'",
            NamedSelection::Field(
                Some(Alias::new("hi")),
                Key::quoted("hello world").into_with_range(),
                None,
            ),
            "hi",
        );

        assert_result_and_name(
            "hi: hello { world }",
            NamedSelection::Field(
                Some(Alias::new("hi")),
                Key::field("hello").into_with_range(),
                Some(SubSelection {
                    selections: vec![NamedSelection::Field(
                        None,
                        Key::field("world").into_with_range(),
                        None,
                    )],
                    ..Default::default()
                }),
            ),
            "hi",
        );

        assert_result_and_name(
            "hey: hello { world again }",
            NamedSelection::Field(
                Some(Alias::new("hey")),
                Key::field("hello").into_with_range(),
                Some(SubSelection {
                    selections: vec![
                        NamedSelection::Field(None, Key::field("world").into_with_range(), None),
                        NamedSelection::Field(None, Key::field("again").into_with_range(), None),
                    ],
                    ..Default::default()
                }),
            ),
            "hey",
        );

        assert_result_and_name(
            "hey: 'hello world' { again }",
            NamedSelection::Field(
                Some(Alias::new("hey")),
                Key::quoted("hello world").into_with_range(),
                Some(SubSelection {
                    selections: vec![NamedSelection::Field(
                        None,
                        Key::field("again").into_with_range(),
                        None,
                    )],
                    ..Default::default()
                }),
            ),
            "hey",
        );

        assert_result_and_name(
            "leggo: 'my ego'",
            NamedSelection::Field(
                Some(Alias::new("leggo")),
                Key::quoted("my ego").into_with_range(),
                None,
            ),
            "leggo",
        );

        assert_result_and_name(
            "'let go': 'my ego'",
            NamedSelection::Field(
                Some(Alias::quoted("let go")),
                Key::quoted("my ego").into_with_range(),
                None,
            ),
            "let go",
        );
    }

    #[test]
    fn test_selection() {
        assert_eq!(
            selection!("").strip_ranges(),
            JSONSelection::Named(SubSelection {
                selections: vec![],
                ..Default::default()
            }),
        );

        assert_eq!(
            selection!("   ").strip_ranges(),
            JSONSelection::Named(SubSelection {
                selections: vec![],
                ..Default::default()
            }),
        );

        assert_eq!(
            selection!("hello").strip_ranges(),
            JSONSelection::Named(SubSelection {
                selections: vec![NamedSelection::Field(
                    None,
                    Key::field("hello").into_with_range(),
                    None
                )],
                ..Default::default()
            }),
        );

        assert_eq!(
            selection!(".hello").strip_ranges(),
            JSONSelection::Path(PathSelection::from_slice(
                &[Key::Field("hello".to_string())],
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
                ..Default::default()
            });

            assert_eq!(selection!("hi: .hello.world").strip_ranges(), expected);
            assert_eq!(selection!("hi: .hello .world").strip_ranges(), expected);
            assert_eq!(selection!("hi: . hello. world").strip_ranges(), expected);
            assert_eq!(selection!("hi: .hello . world").strip_ranges(), expected);
            assert_eq!(selection!("hi: hello.world").strip_ranges(), expected);
            assert_eq!(selection!("hi: hello. world").strip_ranges(), expected);
            assert_eq!(selection!("hi: hello .world").strip_ranges(), expected);
            assert_eq!(selection!("hi: hello . world").strip_ranges(), expected);
        }

        {
            let expected = JSONSelection::Named(SubSelection {
                selections: vec![
                    NamedSelection::Field(None, Key::field("before").into_with_range(), None),
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
                    NamedSelection::Field(None, Key::field("after").into_with_range(), None),
                ],
                ..Default::default()
            });

            assert_eq!(
                selection!("before hi: .hello.world after").strip_ranges(),
                expected
            );
            assert_eq!(
                selection!("before hi: .hello .world after").strip_ranges(),
                expected
            );
            assert_eq!(
                selection!("before hi: .hello. world after").strip_ranges(),
                expected
            );
            assert_eq!(
                selection!("before hi: .hello . world after").strip_ranges(),
                expected
            );
            assert_eq!(
                selection!("before hi: . hello.world after").strip_ranges(),
                expected
            );
            assert_eq!(
                selection!("before hi: . hello .world after").strip_ranges(),
                expected
            );
            assert_eq!(
                selection!("before hi: . hello. world after").strip_ranges(),
                expected
            );
            assert_eq!(
                selection!("before hi: . hello . world after").strip_ranges(),
                expected
            );
            assert_eq!(
                selection!("before hi: hello.world after").strip_ranges(),
                expected
            );
            assert_eq!(
                selection!("before hi: hello .world after").strip_ranges(),
                expected
            );
            assert_eq!(
                selection!("before hi: hello. world after").strip_ranges(),
                expected
            );
            assert_eq!(
                selection!("before hi: hello . world after").strip_ranges(),
                expected
            );
        }

        {
            let expected = JSONSelection::Named(SubSelection {
                selections: vec![
                    NamedSelection::Field(None, Key::field("before").into_with_range(), None),
                    NamedSelection::Path(
                        Alias::new("hi"),
                        PathSelection::from_slice(
                            &[
                                Key::Field("hello".to_string()),
                                Key::Field("world".to_string()),
                            ],
                            Some(SubSelection {
                                selections: vec![
                                    NamedSelection::Field(
                                        None,
                                        Key::field("nested").into_with_range(),
                                        None,
                                    ),
                                    NamedSelection::Field(
                                        None,
                                        Key::field("names").into_with_range(),
                                        None,
                                    ),
                                ],
                                ..Default::default()
                            }),
                        ),
                    ),
                    NamedSelection::Field(None, Key::field("after").into_with_range(), None),
                ],
                ..Default::default()
            });

            assert_eq!(
                selection!("before hi: .hello.world { nested names } after").strip_ranges(),
                expected
            );
            assert_eq!(
                selection!("before hi:.hello.world{nested names}after").strip_ranges(),
                expected
            );
            assert_eq!(
                selection!("before hi: hello.world { nested names } after").strip_ranges(),
                expected
            );
            assert_eq!(
                selection!("before hi:hello.world{nested names}after").strip_ranges(),
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
            )
            .strip_ranges(),
            JSONSelection::Named(SubSelection {
                selections: vec![NamedSelection::Field(
                    Some(Alias::new("topLevelAlias")),
                    Key::field("topLevelField").into_with_range(),
                    Some(SubSelection {
                        selections: vec![
                            NamedSelection::Field(
                                Some(Alias::new("identifier")),
                                Key::quoted("property name with spaces").into_with_range(),
                                None,
                            ),
                            NamedSelection::Field(
                                None,
                                Key::quoted("unaliased non-identifier property").into_with_range(),
                                None,
                            ),
                            NamedSelection::Field(
                                Some(Alias::quoted("non-identifier alias")),
                                Key::field("identifier").into_with_range(),
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
                                                Key::field("yet").into_with_range(),
                                                None,
                                            ),
                                            NamedSelection::Field(
                                                None,
                                                Key::field("more").into_with_range(),
                                                None,
                                            ),
                                            NamedSelection::Field(
                                                None,
                                                Key::field("properties").into_with_range(),
                                                None,
                                            ),
                                        ],
                                        ..Default::default()
                                    })
                                ),
                            ),
                            NamedSelection::Group(
                                Alias::new("siblingGroup"),
                                SubSelection {
                                    selections: vec![
                                        NamedSelection::Field(
                                            None,
                                            Key::field("brother").into_with_range(),
                                            None
                                        ),
                                        NamedSelection::Field(
                                            None,
                                            Key::field("sister").into_with_range(),
                                            None
                                        ),
                                    ],
                                    ..Default::default()
                                },
                            ),
                        ],
                        ..Default::default()
                    }),
                )],
                ..Default::default()
            }),
        );
    }

    fn check_path_selection(input: &str, expected: PathSelection) {
        let (remainder, path_selection) = PathSelection::parse(Span::new(input)).unwrap();
        assert!(span_is_all_spaces_or_comments(remainder));
        assert_eq!(&path_selection.strip_ranges(), &expected);
        assert_eq!(
            selection!(input).strip_ranges(),
            JSONSelection::Path(expected)
        );
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
                    selections: vec![NamedSelection::Field(
                        None,
                        Key::field("hello").into_with_range(),
                        None,
                    )],
                    ..Default::default()
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
                        Key::quoted("my ego").into_with_range(),
                        None,
                    )],
                    ..Default::default()
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
                    Key::field("results").into_with_range(),
                    PathList::Selection(SubSelection {
                        selections: vec![NamedSelection::Field(
                            None,
                            Key::quoted("quoted without alias").into_with_range(),
                            Some(SubSelection {
                                selections: vec![
                                    NamedSelection::Field(
                                        None,
                                        Key::field("id").into_with_range(),
                                        None,
                                    ),
                                    NamedSelection::Field(
                                        None,
                                        Key::quoted("n a m e").into_with_range(),
                                        None,
                                    ),
                                ],
                                ..Default::default()
                            }),
                        )],
                        ..Default::default()
                    })
                    .into_with_range(),
                )
                .into_with_range(),
            };
            check_path_selection(
                ".results { 'quoted without alias' { id 'n a m e' } }",
                expected.clone(),
            );
            check_path_selection(
                ".results{'quoted without alias'{id'n a m e'}}",
                expected.clone(),
            );
        }

        {
            let expected = PathSelection {
                path: PathList::Key(
                    Key::field("results").into_with_range(),
                    PathList::Selection(SubSelection {
                        selections: vec![NamedSelection::Field(
                            Some(Alias::quoted("non-identifier alias")),
                            Key::quoted("quoted with alias").into_with_range(),
                            Some(SubSelection {
                                selections: vec![
                                    NamedSelection::Field(
                                        None,
                                        Key::field("id").into_with_range(),
                                        None,
                                    ),
                                    NamedSelection::Field(
                                        Some(Alias::quoted("n a m e")),
                                        Key::field("name").into_with_range(),
                                        None,
                                    ),
                                ],
                                ..Default::default()
                            }),
                        )],
                        ..Default::default()
                    })
                    .into_with_range(),
                )
                .into_with_range(),
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
            PathSelection {
                path: PathList::Var(
                    KnownVariable::This.into_with_range(),
                    PathList::Empty.into_with_range(),
                )
                .into_with_range(),
            },
        );

        check_path_selection(
            "$",
            PathSelection {
                path: PathList::Var(
                    KnownVariable::Dollar.into_with_range(),
                    PathList::Empty.into_with_range(),
                )
                .into_with_range(),
            },
        );

        check_path_selection(
            "$this { hello }",
            PathSelection {
                path: PathList::Var(
                    KnownVariable::This.into_with_range(),
                    PathList::Selection(SubSelection {
                        selections: vec![NamedSelection::Field(
                            None,
                            Key::field("hello").into_with_range(),
                            None,
                        )],
                        ..Default::default()
                    })
                    .into_with_range(),
                )
                .into_with_range(),
            },
        );

        check_path_selection(
            "$ { hello }",
            PathSelection {
                path: PathList::Var(
                    KnownVariable::Dollar.into_with_range(),
                    PathList::Selection(SubSelection {
                        selections: vec![NamedSelection::Field(
                            None,
                            Key::field("hello").into_with_range(),
                            None,
                        )],
                        ..Default::default()
                    })
                    .into_with_range(),
                )
                .into_with_range(),
            },
        );

        check_path_selection(
            "$this { before alias: $args.arg after }",
            PathList::Var(
                KnownVariable::This.into_with_range(),
                PathList::Selection(SubSelection {
                    selections: vec![
                        NamedSelection::Field(None, Key::field("before").into_with_range(), None),
                        NamedSelection::Path(
                            Alias::new("alias"),
                            PathSelection {
                                path: PathList::Var(
                                    KnownVariable::Args.into_with_range(),
                                    PathList::Key(
                                        Key::field("arg").into_with_range(),
                                        PathList::Empty.into_with_range(),
                                    )
                                    .into_with_range(),
                                )
                                .into_with_range(),
                            },
                        ),
                        NamedSelection::Field(None, Key::field("after").into_with_range(), None),
                    ],
                    ..Default::default()
                })
                .into_with_range(),
            )
            .into(),
        );

        check_path_selection(
            "$.nested { key injected: $args.arg }",
            PathSelection {
                path: PathList::Var(
                    KnownVariable::Dollar.into_with_range(),
                    PathList::Key(
                        Key::field("nested").into_with_range(),
                        PathList::Selection(SubSelection {
                            selections: vec![
                                NamedSelection::Field(
                                    None,
                                    Key::field("key").into_with_range(),
                                    None,
                                ),
                                NamedSelection::Path(
                                    Alias::new("injected"),
                                    PathSelection {
                                        path: PathList::Var(
                                            KnownVariable::Args.into_with_range(),
                                            PathList::Key(
                                                Key::field("arg").into_with_range(),
                                                PathList::Empty.into_with_range(),
                                            )
                                            .into_with_range(),
                                        )
                                        .into_with_range(),
                                    },
                                ),
                            ],
                            ..Default::default()
                        })
                        .into_with_range(),
                    )
                    .into_with_range(),
                )
                .into_with_range(),
            },
        );

        check_path_selection(
            "$args.a.b.c",
            PathSelection {
                path: PathList::Var(
                    KnownVariable::Args.into_with_range(),
                    PathList::from_slice(
                        &[
                            Key::Field("a".to_string()),
                            Key::Field("b".to_string()),
                            Key::Field("c".to_string()),
                        ],
                        None,
                    )
                    .into_with_range(),
                )
                .into_with_range(),
            },
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
            PathSelection {
                path: PathList::Var(
                    KnownVariable::Dollar.into_with_range(),
                    PathList::Key(
                        Key::field("data").into_with_range(),
                        PathList::Empty.into_with_range(),
                    )
                    .into_with_range(),
                )
                .into_with_range(),
            },
        );

        check_path_selection(
            "$.data.'quoted property'.nested",
            PathSelection {
                path: PathList::Var(
                    KnownVariable::Dollar.into_with_range(),
                    PathList::Key(
                        Key::field("data").into_with_range(),
                        PathList::Key(
                            Key::quoted("quoted property").into_with_range(),
                            PathList::Key(
                                Key::field("nested").into_with_range(),
                                PathList::Empty.into_with_range(),
                            )
                            .into_with_range(),
                        )
                        .into_with_range(),
                    )
                    .into_with_range(),
                )
                .into_with_range(),
            },
        );

        {
            let input = Span::new("naked");
            assert_eq!(
                PathSelection::parse(input),
                Err(nom::Err::Error(nom::error::Error::new(
                    input.slice(5..),
                    nom::error::ErrorKind::IsNot,
                ))),
            );
        }

        {
            let input = Span::new("naked { hi }");
            assert_eq!(
                PathSelection::parse(input),
                Err(nom::Err::Error(nom::error::Error::new(
                    input.slice(12..),
                    nom::error::ErrorKind::IsNot,
                ))),
            );
        }

        {
            let input = Span::new("valid.$invalid");
            assert_eq!(
                PathSelection::parse(input),
                Err(nom::Err::Error(nom::error::Error::new(
                    input.slice(5..),
                    nom::error::ErrorKind::IsNot,
                ))),
            );
        }

        assert_eq!(
            selection!("$").strip_ranges(),
            JSONSelection::Path(PathSelection {
                path: PathList::Var(
                    KnownVariable::Dollar.into_with_range(),
                    PathList::Empty.into_with_range()
                )
                .into_with_range(),
            }),
        );

        assert_eq!(
            selection!("$this").strip_ranges(),
            JSONSelection::Path(PathSelection {
                path: PathList::Var(
                    KnownVariable::This.into_with_range(),
                    PathList::Empty.into_with_range()
                )
                .into_with_range(),
            }),
        );

        assert_eq!(
            selection!("value: $ a { b c }").strip_ranges(),
            JSONSelection::Named(SubSelection {
                selections: vec![
                    NamedSelection::Path(
                        Alias::new("value"),
                        PathSelection {
                            path: PathList::Var(
                                KnownVariable::Dollar.into_with_range(),
                                PathList::Empty.into_with_range()
                            )
                            .into_with_range(),
                        },
                    ),
                    NamedSelection::Field(
                        None,
                        Key::field("a").into_with_range(),
                        Some(SubSelection {
                            selections: vec![
                                NamedSelection::Field(
                                    None,
                                    Key::field("b").into_with_range(),
                                    None
                                ),
                                NamedSelection::Field(
                                    None,
                                    Key::field("c").into_with_range(),
                                    None
                                ),
                            ],
                            ..Default::default()
                        }),
                    ),
                ],
                ..Default::default()
            }),
        );
        assert_eq!(
            selection!("value: $this { b c }").strip_ranges(),
            JSONSelection::Named(SubSelection {
                selections: vec![NamedSelection::Path(
                    Alias::new("value"),
                    PathSelection {
                        path: PathList::Var(
                            KnownVariable::This.into_with_range(),
                            PathList::Selection(SubSelection {
                                selections: vec![
                                    NamedSelection::Field(
                                        None,
                                        Key::field("b").into_with_range(),
                                        None
                                    ),
                                    NamedSelection::Field(
                                        None,
                                        Key::field("c").into_with_range(),
                                        None
                                    ),
                                ],
                                ..Default::default()
                            })
                            .into_with_range(),
                        )
                        .into_with_range(),
                    },
                )],
                ..Default::default()
            }),
        );
    }

    #[test]
    fn test_path_selection_at() {
        check_path_selection(
            "@",
            PathSelection {
                path: PathList::Var(
                    KnownVariable::AtSign.into_with_range(),
                    PathList::Empty.into_with_range(),
                )
                .into_with_range(),
            },
        );

        check_path_selection(
            "@.a.b.c",
            PathSelection {
                path: PathList::Var(
                    KnownVariable::AtSign.into_with_range(),
                    PathList::from_slice(
                        &[
                            Key::Field("a".to_string()),
                            Key::Field("b".to_string()),
                            Key::Field("c".to_string()),
                        ],
                        None,
                    )
                    .into_with_range(),
                )
                .into_with_range(),
            },
        );

        check_path_selection(
            "@.items->first",
            PathSelection {
                path: PathList::Var(
                    KnownVariable::AtSign.into_with_range(),
                    PathList::Key(
                        Key::field("items").into_with_range(),
                        PathList::Method(
                            WithRange::new("first".to_string(), None),
                            None,
                            PathList::Empty.into_with_range(),
                        )
                        .into_with_range(),
                    )
                    .into_with_range(),
                )
                .into_with_range(),
            },
        );
    }

    #[test]
    fn test_path_methods() {
        check_path_selection(
            "data.x->or(data.y)",
            PathSelection {
                path: PathList::Key(
                    Key::field("data").into_with_range(),
                    PathList::Key(
                        Key::field("x").into_with_range(),
                        PathList::Method(
                            WithRange::new("or".to_string(), None),
                            Some(MethodArgs {
                                args: vec![LitExpr::Path(PathSelection::from_slice(
                                    &[Key::field("data"), Key::field("y")],
                                    None,
                                ))
                                .into_with_range()],
                                ..Default::default()
                            }),
                            PathList::Empty.into_with_range(),
                        )
                        .into_with_range(),
                    )
                    .into_with_range(),
                )
                .into_with_range(),
            },
        );

        {
            let expected = PathSelection {
                path: PathList::Key(
                    Key::field("data").into_with_range(),
                    PathList::Method(
                        WithRange::new("query".to_string(), None),
                        Some(MethodArgs {
                            args: vec![
                                LitExpr::Path(PathSelection::from_slice(&[Key::field("a")], None))
                                    .into_with_range(),
                                LitExpr::Path(PathSelection::from_slice(&[Key::field("b")], None))
                                    .into_with_range(),
                                LitExpr::Path(PathSelection::from_slice(&[Key::field("c")], None))
                                    .into_with_range(),
                            ],
                            ..Default::default()
                        }),
                        PathList::Empty.into_with_range(),
                    )
                    .into_with_range(),
                )
                .into_with_range(),
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
                    Key::field("data").into_with_range(),
                    PathList::Key(
                        Key::field("x").into_with_range(),
                        PathList::Method(
                            WithRange::new("concat".to_string(), None),
                            Some(MethodArgs {
                                args: vec![LitExpr::Array(vec![
                                    LitExpr::Path(PathSelection::from_slice(
                                        &[Key::field("data"), Key::field("y")],
                                        None,
                                    ))
                                    .into_with_range(),
                                    LitExpr::Path(PathSelection::from_slice(
                                        &[Key::field("data"), Key::field("z")],
                                        None,
                                    ))
                                    .into_with_range(),
                                ])
                                .into_with_range()],
                                ..Default::default()
                            }),
                            PathList::Empty.into_with_range(),
                        )
                        .into_with_range(),
                    )
                    .into_with_range(),
                )
                .into_with_range(),
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
                    Key::field("data").into_with_range(),
                    PathList::Method(
                        WithRange::new("method".to_string(), None),
                        Some(MethodArgs {
                                args: vec![LitExpr::Array(vec![
                                LitExpr::Path(PathSelection {
                                    path: PathList::Var(
                                        KnownVariable::Dollar.into_with_range(),
                                        PathList::Selection(
                                            SubSelection {
                                                selections: vec![NamedSelection::Path(
                                                    Alias::new("x2"),
                                                    PathSelection {
                                                        path: PathList::Key(
                                                            Key::field("x").into_with_range(),
                                                            PathList::Method(
                                                                WithRange::new(
                                                                    "times".to_string(),
                                                                    None,
                                                                ),
                                                                Some(MethodArgs {
                                                                    args: vec![LitExpr::Number(
                                                                        "2".parse().expect(
                                                                            "serde_json::Number parse error",
                                                                        ),
                                                                    ).into_with_range()],
                                                                    ..Default::default()
                                                                }),
                                                                PathList::Empty.into_with_range(),
                                                            )
                                                            .into_with_range(),
                                                        )
                                                        .into_with_range(),
                                                    },
                                                )],
                                                ..Default::default()
                                            },
                                        )
                                        .into_with_range(),
                                    )
                                    .into_with_range(),
                                })
                                .into_with_range(),
                                LitExpr::Path(PathSelection {
                                    path: PathList::Var(
                                        KnownVariable::Dollar.into_with_range(),
                                        PathList::Selection(
                                            SubSelection {
                                                selections: vec![NamedSelection::Path(
                                                    Alias::new("y2"),
                                                    PathSelection {
                                                        path: PathList::Key(
                                                            Key::field("y").into_with_range(),
                                                            PathList::Method(
                                                                WithRange::new(
                                                                    "times".to_string(),
                                                                    None,
                                                                ),
                                                                Some(
                                                                    MethodArgs {
                                                                        args: vec![LitExpr::Number(
                                                                            "2".parse().expect(
                                                                                "serde_json::Number parse error",
                                                                            ),
                                                                        ).into_with_range()],
                                                                        ..Default::default()
                                                                    },
                                                                ),
                                                                PathList::Empty.into_with_range(),
                                                            )
                                                            .into_with_range(),
                                                        )
                                                        .into_with_range(),
                                                    },
                                                )],
                                                ..Default::default()
                                            },
                                        )
                                        .into_with_range(),
                                    )
                                    .into_with_range(),
                                })
                                .into_with_range(),
                            ])
                            .into_with_range()],
                            ..Default::default()
                        }),
                        PathList::Empty.into_with_range(),
                    )
                    .into_with_range(),
                )
                .into_with_range(),
            },
        );
    }

    #[test]
    fn test_subselection() {
        fn check_parsed(input: &str, expected: SubSelection) {
            let (remainder, parsed) = SubSelection::parse(Span::new(input)).unwrap();
            assert!(span_is_all_spaces_or_comments(remainder));
            assert_eq!(parsed.strip_ranges(), expected);
        }

        check_parsed(
            " { \n } ",
            SubSelection {
                selections: vec![],
                ..Default::default()
            },
        );

        check_parsed(
            "{hello}",
            SubSelection {
                selections: vec![NamedSelection::Field(
                    None,
                    Key::field("hello").into_with_range(),
                    None,
                )],
                ..Default::default()
            },
        );

        check_parsed(
            "{ hello }",
            SubSelection {
                selections: vec![NamedSelection::Field(
                    None,
                    Key::field("hello").into_with_range(),
                    None,
                )],
                ..Default::default()
            },
        );

        check_parsed(
            "  { padded  } ",
            SubSelection {
                selections: vec![NamedSelection::Field(
                    None,
                    Key::field("padded").into_with_range(),
                    None,
                )],
                ..Default::default()
            },
        );

        check_parsed(
            "{ hello world }",
            SubSelection {
                selections: vec![
                    NamedSelection::Field(None, Key::field("hello").into_with_range(), None),
                    NamedSelection::Field(None, Key::field("world").into_with_range(), None),
                ],
                ..Default::default()
            },
        );

        check_parsed(
            "{ hello { world } }",
            SubSelection {
                selections: vec![NamedSelection::Field(
                    None,
                    Key::field("hello").into_with_range(),
                    Some(SubSelection {
                        selections: vec![NamedSelection::Field(
                            None,
                            Key::field("world").into_with_range(),
                            None,
                        )],
                        ..Default::default()
                    }),
                )],
                ..Default::default()
            },
        );
    }

    #[test]
    fn test_star_selection() {
        fn check_parsed(input: &str, expected: StarSelection) {
            let (remainder, parsed) = StarSelection::parse(Span::new(input)).unwrap();
            assert!(span_is_all_spaces_or_comments(remainder));
            assert_eq!(parsed.strip_ranges(), expected);
        }

        check_parsed(
            "rest: *",
            StarSelection {
                alias: Some(Alias::new("rest")),
                ..Default::default()
            },
        );

        check_parsed(
            "*",
            StarSelection {
                ..Default::default()
            },
        );

        check_parsed(
            " * ",
            StarSelection {
                ..Default::default()
            },
        );
        check_parsed(
            " * { hello } ",
            StarSelection {
                selection: Some(Box::new(SubSelection {
                    selections: vec![NamedSelection::Field(
                        None,
                        Key::field("hello").into_with_range(),
                        None,
                    )],
                    ..Default::default()
                })),
                ..Default::default()
            },
        );

        check_parsed(
            "hi: * { hello }",
            StarSelection {
                alias: Some(Alias::new("hi")),
                selection: Some(Box::new(SubSelection {
                    selections: vec![NamedSelection::Field(
                        None,
                        Key::field("hello").into_with_range(),
                        None,
                    )],
                    ..Default::default()
                })),
                ..Default::default()
            },
        );

        check_parsed(
            "alias: * { x y z rest: * }",
            StarSelection {
                alias: Some(Alias::new("alias")),
                selection: Some(Box::new(SubSelection {
                    selections: vec![
                        NamedSelection::Field(None, Key::field("x").into_with_range(), None),
                        NamedSelection::Field(None, Key::field("y").into_with_range(), None),
                        NamedSelection::Field(None, Key::field("z").into_with_range(), None),
                    ],
                    star: Some(StarSelection {
                        alias: Some(Alias::new("rest")),
                        ..Default::default()
                    }),
                    ..Default::default()
                })),
                ..Default::default()
            },
        );

        assert_eq!(
            selection!(" before alias: * { * { a b c } } ").strip_ranges(),
            JSONSelection::Named(SubSelection {
                selections: vec![NamedSelection::Field(
                    None,
                    Key::field("before").into_with_range(),
                    None
                )],
                star: Some(StarSelection {
                    alias: Some(Alias::new("alias")),
                    selection: Some(Box::new(SubSelection {
                        selections: vec![],
                        star: Some(StarSelection {
                            selection: Some(Box::new(SubSelection {
                                selections: vec![
                                    NamedSelection::Field(
                                        None,
                                        Key::field("a").into_with_range(),
                                        None
                                    ),
                                    NamedSelection::Field(
                                        None,
                                        Key::field("b").into_with_range(),
                                        None
                                    ),
                                    NamedSelection::Field(
                                        None,
                                        Key::field("c").into_with_range(),
                                        None
                                    ),
                                ],
                                ..Default::default()
                            })),
                            ..Default::default()
                        }),
                        ..Default::default()
                    })),
                    ..Default::default()
                }),
                ..Default::default()
            }),
        );

        assert_eq!(
            selection!(" before group: { * { a b c } } after ").strip_ranges(),
            JSONSelection::Named(SubSelection {
                selections: vec![
                    NamedSelection::Field(None, Key::field("before").into_with_range(), None),
                    NamedSelection::Group(
                        Alias::new("group"),
                        SubSelection {
                            selections: vec![],
                            star: Some(StarSelection {
                                selection: Some(Box::new(SubSelection {
                                    selections: vec![
                                        NamedSelection::Field(
                                            None,
                                            Key::field("a").into_with_range(),
                                            None
                                        ),
                                        NamedSelection::Field(
                                            None,
                                            Key::field("b").into_with_range(),
                                            None
                                        ),
                                        NamedSelection::Field(
                                            None,
                                            Key::field("c").into_with_range(),
                                            None
                                        ),
                                    ],
                                    ..Default::default()
                                })),
                                ..Default::default()
                            }),
                            ..Default::default()
                        },
                    ),
                    NamedSelection::Field(None, Key::field("after").into_with_range(), None),
                ],
                ..Default::default()
            }),
        );
    }

    #[test]
    fn test_external_var_paths() {
        fn parse(input: &str) -> PathSelection {
            PathSelection::parse(Span::new(input))
                .unwrap()
                .1
                .strip_ranges()
        }

        {
            let sel = selection!(
                r#"
                $->echo([$args.arg1, $args.arg2, @.items->first])
            "#
            )
            .strip_ranges();
            let args_arg1_path = parse("$args.arg1");
            let args_arg2_path = parse("$args.arg2");
            assert_eq!(
                sel.external_var_paths(),
                vec![&args_arg1_path, &args_arg2_path]
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
            )
            .strip_ranges();
            let this_kind_path = match &sel {
                JSONSelection::Path(path) => path,
                _ => panic!("Expected PathSelection"),
            };
            let this_a_path = parse("$this.a");
            let this_b_path = parse("$this.b");
            let this_c_path = parse("$this.c");
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
            )
            .strip_ranges();
            let start_path = parse("$args.start");
            let end_path = parse("$args.end");
            let args_type_path = parse("$args.type");
            assert_eq!(
                sel.external_var_paths(),
                vec![&start_path, &end_path, &args_type_path]
            );
        }
    }

    #[test]
    fn test_ranged_locations() {
        fn check(input: &str, expected: JSONSelection) {
            let (remainder, parsed) = JSONSelection::parse(input).unwrap();
            // We can't (and don't want to) use span_is_all_spaces_or_comments
            // here because remainder is a &str not a Span, and
            // JSONSelection::parse is responsible for consuming the entire
            // string when possible (see all_consuming).
            assert_eq!(remainder, "");
            assert_eq!(parsed, expected);
        }

        check(
            "hello",
            JSONSelection::Named(SubSelection {
                selections: vec![NamedSelection::Field(
                    None,
                    WithRange::new(Key::field("hello"), Some(0..5)),
                    None,
                )],
                range: Some(0..5),
                ..Default::default()
            }),
        );

        check(
            "  hello ",
            JSONSelection::Named(SubSelection {
                selections: vec![NamedSelection::Field(
                    None,
                    WithRange::new(Key::field("hello"), Some(2..7)),
                    None,
                )],
                range: Some(2..7),
                ..Default::default()
            }),
        );

        check(
            "  hello  { hi name }",
            JSONSelection::Named(SubSelection {
                selections: vec![NamedSelection::Field(
                    None,
                    WithRange::new(Key::field("hello"), Some(2..7)),
                    Some(SubSelection {
                        selections: vec![
                            NamedSelection::Field(
                                None,
                                WithRange::new(Key::field("hi"), Some(11..13)),
                                None,
                            ),
                            NamedSelection::Field(
                                None,
                                WithRange::new(Key::field("name"), Some(14..18)),
                                None,
                            ),
                        ],
                        range: Some(9..20),
                        ..Default::default()
                    }),
                )],
                range: Some(2..20),
                ..Default::default()
            }),
        );

        check(
            "$args.product.id",
            JSONSelection::Path(PathSelection {
                path: WithRange::new(
                    PathList::Var(
                        WithRange::new(KnownVariable::Args, Some(0..5)),
                        WithRange::new(
                            PathList::Key(
                                WithRange::new(Key::field("product"), Some(6..13)),
                                WithRange::new(
                                    PathList::Key(
                                        WithRange::new(Key::field("id"), Some(14..16)),
                                        WithRange::new(PathList::Empty, Some(16..16)),
                                    ),
                                    Some(13..16),
                                ),
                            ),
                            Some(5..16),
                        ),
                    ),
                    Some(0..16),
                ),
            }),
        );

        check(
            " $args . product . id ",
            JSONSelection::Path(PathSelection {
                path: WithRange::new(
                    PathList::Var(
                        WithRange::new(KnownVariable::Args, Some(1..6)),
                        WithRange::new(
                            PathList::Key(
                                WithRange::new(Key::field("product"), Some(9..16)),
                                WithRange::new(
                                    PathList::Key(
                                        WithRange::new(Key::field("id"), Some(19..21)),
                                        WithRange::new(PathList::Empty, Some(21..21)),
                                    ),
                                    Some(17..21),
                                ),
                            ),
                            Some(7..21),
                        ),
                    ),
                    Some(1..21),
                ),
            }),
        );

        check(
            "before product:$args.product{id name}after",
            JSONSelection::Named(SubSelection {
                selections: vec![
                    NamedSelection::Field(
                        None,
                        WithRange::new(Key::field("before"), Some(0..6)),
                        None,
                    ),
                    NamedSelection::Path(
                        Alias {
                            name: WithRange::new(Key::field("product"), Some(7..14)),
                            range: Some(7..15),
                        },
                        PathSelection {
                            path: WithRange::new(
                                PathList::Var(
                                    WithRange::new(KnownVariable::Args, Some(15..20)),
                                    WithRange::new(
                                        PathList::Key(
                                            WithRange::new(Key::field("product"), Some(21..28)),
                                            WithRange::new(
                                                PathList::Selection(SubSelection {
                                                    selections: vec![
                                                        NamedSelection::Field(
                                                            None,
                                                            WithRange::new(
                                                                Key::field("id"),
                                                                Some(29..31),
                                                            ),
                                                            None,
                                                        ),
                                                        NamedSelection::Field(
                                                            None,
                                                            WithRange::new(
                                                                Key::field("name"),
                                                                Some(32..36),
                                                            ),
                                                            None,
                                                        ),
                                                    ],
                                                    range: Some(28..37),
                                                    ..Default::default()
                                                }),
                                                Some(28..37),
                                            ),
                                        ),
                                        Some(20..37),
                                    ),
                                ),
                                Some(15..37),
                            ),
                        },
                    ),
                    NamedSelection::Field(
                        None,
                        WithRange::new(Key::field("after"), Some(37..42)),
                        None,
                    ),
                ],
                range: Some(0..42),
                ..Default::default()
            }),
        );
    }
}
