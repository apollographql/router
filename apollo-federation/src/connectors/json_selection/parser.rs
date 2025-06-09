use std::fmt::Display;
use std::hash::Hash;
use std::str::FromStr;

use itertools::Itertools;
use nom::IResult;
use nom::Slice;
use nom::branch::alt;
use nom::character::complete::char;
use nom::character::complete::one_of;
use nom::combinator::all_consuming;
use nom::combinator::map;
use nom::combinator::opt;
use nom::combinator::recognize;
use nom::error::ParseError;
use nom::multi::many0;
use nom::sequence::pair;
use nom::sequence::preceded;
use nom::sequence::terminated;
use nom::sequence::tuple;
use serde_json_bytes::Value as JSON;

use super::helpers::spaces_or_comments;
use super::known_var::KnownVariable;
use super::lit_expr::LitExpr;
use super::location::OffsetRange;
use super::location::Ranged;
use super::location::Span;
use super::location::WithRange;
use super::location::merge_ranges;
use super::location::new_span;
use super::location::ranged_span;
use crate::connectors::Namespace;
use crate::connectors::variable::VariableNamespace;
use crate::connectors::variable::VariableReference;

// ParseResult is the internal type returned by most ::parse methods, as it is
// convenient to use with nom's combinators. The top-level JSONSelection::parse
// method returns a slightly different IResult type that hides implementation
// details of the nom-specific types.
//
// TODO Consider switching the third IResult type parameter to VerboseError
// here, if error messages can be improved with additional context.
pub(super) type ParseResult<'a, T> = IResult<Span<'a>, T>;

// Generates a non-fatal error with the given suffix and message, allowing the
// parser to recover and continue.
pub(super) fn nom_error_message<'a>(
    suffix: Span<'a>,
    // This message type forbids computing error messages with format!, which
    // might be worthwhile in the future. For now, it's convenient to avoid
    // String messages so the Span type can remain Copy, so we don't have to
    // clone spans frequently in the parsing code. In most cases, the suffix
    // provides the dynamic context needed to interpret the static message.
    message: &'static str,
) -> nom::Err<nom::error::Error<Span<'a>>> {
    nom::Err::Error(nom::error::Error::from_error_kind(
        suffix.map_extra(|_| Some(message)),
        nom::error::ErrorKind::IsNot,
    ))
}

// Generates a fatal error with the given suffix Span and message, causing the
// parser to abort with the given error message, which is useful after
// recognizing syntax that completely constrains what follows (like the -> token
// before a method name), and what follows does not parse as required.
pub(super) fn nom_fail_message<'a>(
    suffix: Span<'a>,
    message: &'static str,
) -> nom::Err<nom::error::Error<Span<'a>>> {
    nom::Err::Failure(nom::error::Error::from_error_kind(
        suffix.map_extra(|_| Some(message)),
        nom::error::ErrorKind::IsNot,
    ))
}

pub(crate) trait ExternalVarPaths {
    fn external_var_paths(&self) -> Vec<&PathSelection>;
}

// JSONSelection     ::= PathSelection | NakedSubSelection
// NakedSubSelection ::= NamedSelection* StarSelection?

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum JSONSelection {
    // Although we reuse the SubSelection type for the JSONSelection::Named
    // case, we parse it as a sequence of NamedSelection items without the
    // {...} curly braces that SubSelection::parse expects.
    Named(SubSelection),
    Path(PathSelection),
}

// To keep JSONSelection::parse consumers from depending on details of the nom
// error types, JSONSelection::parse reports this custom error type. Other
// ::parse methods still internally report nom::error::Error for the most part.
#[derive(thiserror::Error, Debug, PartialEq, Eq, Clone)]
#[error("{message}: {fragment}")]
pub struct JSONSelectionParseError {
    // The message will be a meaningful error message in many cases, but may
    // fall back to a formatted nom::error::ErrorKind in some cases, e.g. when
    // an alt(...) runs out of options and we can't determine which underlying
    // error was "most" responsible.
    pub message: String,

    // Since we are not exposing the nom_locate-specific Span type, we report
    // span.fragment() and span.location_offset() here.
    pub fragment: String,

    // While it might be nice to report a range rather than just an offset, not
    // all parsing errors have an unambiguous end offset, so the best we can do
    // is point to the suffix of the input that failed to parse (which
    // corresponds to where the fragment starts).
    pub offset: usize,
}

impl JSONSelection {
    pub fn empty() -> Self {
        JSONSelection::Named(SubSelection::default())
    }

    pub fn is_empty(&self) -> bool {
        match self {
            JSONSelection::Named(subselect) => subselect.selections.is_empty(),
            JSONSelection::Path(path) => *path.path == PathList::Empty,
        }
    }

    // JSONSelection::parse is possibly the "most public" method in the entire
    // file, so it's important that the method signature can remain stable even
    // if we drastically change implementation details. That's why we use &str
    // as the input type and a custom JSONSelectionParseError type as the error
    // type, rather than using Span or nom::error::Error directly.
    pub fn parse(input: &str) -> Result<Self, JSONSelectionParseError> {
        match JSONSelection::parse_span(new_span(input)) {
            Ok((remainder, selection)) => {
                let fragment = remainder.fragment();
                if fragment.is_empty() {
                    Ok(selection)
                } else {
                    Err(JSONSelectionParseError {
                        message: "Unexpected trailing characters".to_string(),
                        fragment: fragment.to_string(),
                        offset: remainder.location_offset(),
                    })
                }
            }

            Err(e) => match e {
                nom::Err::Error(e) | nom::Err::Failure(e) => Err(JSONSelectionParseError {
                    message: e.input.extra.map_or_else(
                        || format!("nom::error::ErrorKind::{:?}", e.code),
                        |message_str| message_str.to_string(),
                    ),
                    fragment: e.input.fragment().to_string(),
                    offset: e.input.location_offset(),
                }),

                nom::Err::Incomplete(_) => unreachable!("nom::Err::Incomplete not expected here"),
            },
        }
    }

    fn parse_span(input: Span) -> ParseResult<Self> {
        match alt((
            all_consuming(terminated(
                map(PathSelection::parse, Self::Path),
                // By convention, most ::parse methods do not consume trailing
                // spaces_or_comments, so we need to consume them here in order
                // to satisfy the all_consuming requirement.
                spaces_or_comments,
            )),
            all_consuming(terminated(
                map(SubSelection::parse_naked, Self::Named),
                // It's tempting to hoist the all_consuming(terminated(...))
                // checks outside the alt((...)) so we only need to handle
                // trailing spaces_or_comments once, but that won't work because
                // the Self::Path case should fail when a single PathSelection
                // cannot be parsed, and that failure typically happens because
                // the PathSelection::parse method does not consume the entire
                // input, which is caught by the first all_consuming above.
                spaces_or_comments,
            )),
        ))(input)
        {
            Ok((remainder, selection)) => {
                if remainder.fragment().is_empty() {
                    Ok((remainder, selection))
                } else {
                    Err(nom_fail_message(
                        // Usually our nom errors report the original input that
                        // failed to parse, but that's not helpful here, since
                        // input corresponds to the entire string, whereas this
                        // error message is reporting junk at the end of the
                        // string that should not be there.
                        remainder,
                        "Unexpected trailing characters",
                    ))
                }
            }
            Err(e) => Err(e),
        }
    }

    pub(crate) fn next_subselection(&self) -> Option<&SubSelection> {
        match self {
            JSONSelection::Named(subselect) => Some(subselect),
            JSONSelection::Path(path) => path.next_subselection(),
        }
    }

    #[allow(unused)]
    pub(crate) fn next_mut_subselection(&mut self) -> Option<&mut SubSelection> {
        match self {
            JSONSelection::Named(subselect) => Some(subselect),
            JSONSelection::Path(path) => path.next_mut_subselection(),
        }
    }

    pub fn variable_references(&self) -> impl Iterator<Item = VariableReference<Namespace>> + '_ {
        self.external_var_paths()
            .into_iter()
            .flat_map(|var_path| var_path.variable_reference())
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

// NamedSelection       ::= NamedPathSelection | PathWithSubSelection | NamedFieldSelection | NamedGroupSelection
// NamedPathSelection   ::= Alias PathSelection
// NamedFieldSelection  ::= Alias? Key SubSelection?
// NamedGroupSelection  ::= Alias SubSelection
// PathSelection        ::= Path SubSelection?
// PathWithSubSelection ::= Path SubSelection

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum NamedSelection {
    Field(Option<Alias>, WithRange<Key>, Option<SubSelection>),
    // Represents either NamedPathSelection or PathWithSubSelection, with the
    // invariant alias.is_some() || path.has_subselection() enforced by
    // NamedSelection::parse_path.
    Path {
        alias: Option<Alias>,
        // True for PathWithSubSelection, and potentially in the future for
        // object/null-returning NamedSelection::Path items that do not have an
        // explicit trailing SubSelection.
        inline: bool,
        path: PathSelection,
    },
    Group(Alias, SubSelection),
}

// Like PathSelection, NamedSelection is an AST structure that takes its range
// entirely from its children, so NamedSelection itself does not need to provide
// separate storage for its own range, and therefore does not need to be wrapped
// as WithRange<NamedSelection>, but merely needs to implement the Ranged trait.
impl Ranged for NamedSelection {
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
            Self::Path { alias, path, .. } => {
                let alias_range = alias.as_ref().and_then(|alias| alias.range());
                merge_ranges(alias_range, path.range())
            }
            Self::Group(alias, sub) => merge_ranges(alias.range(), sub.range()),
        }
    }
}

impl NamedSelection {
    pub(crate) fn parse(input: Span) -> ParseResult<Self> {
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

    fn parse_field(input: Span) -> ParseResult<Self> {
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

    // Parses either NamedPathSelection or PathWithSubSelection.
    fn parse_path(input: Span) -> ParseResult<Self> {
        if let Ok((remainder, alias)) = Alias::parse(input) {
            match PathSelection::parse(remainder) {
                Ok((remainder, path)) => Ok((
                    remainder,
                    Self::Path {
                        alias: Some(alias),
                        inline: false,
                        path,
                    },
                )),
                Err(nom::Err::Failure(e)) => Err(nom::Err::Failure(e)),
                Err(_) => Err(nom_error_message(
                    input,
                    "Path selection alias must be followed by a path",
                )),
            }
        } else {
            match PathSelection::parse(input) {
                Ok((remainder, path)) => {
                    if path.has_subselection() {
                        Ok((
                            remainder,
                            Self::Path {
                                alias: None,
                                // Inline without ...
                                inline: true,
                                path,
                            },
                        ))
                    } else {
                        Err(nom_fail_message(
                            input,
                            "Named path selection must either begin with alias or ..., or end with subselection",
                        ))
                    }
                }
                Err(nom::Err::Failure(e)) => Err(nom::Err::Failure(e)),
                Err(_) => Err(nom_error_message(
                    input,
                    "Path selection must either begin with alias or ..., or end with subselection",
                )),
            }
        }
    }

    fn parse_group(input: Span) -> ParseResult<Self> {
        tuple((Alias::parse, SubSelection::parse))(input)
            .map(|(input, (alias, group))| (input, Self::Group(alias, group)))
    }

    pub(crate) fn names(&self) -> Vec<&str> {
        match self {
            Self::Field(alias, name, _) => alias
                .as_ref()
                .map(|alias| vec![alias.name.as_str()])
                .unwrap_or_else(|| vec![name.as_str()]),
            Self::Path { alias, path, .. } => {
                if let Some(alias) = alias {
                    vec![alias.name.as_str()]
                } else if let Some(sub) = path.next_subselection() {
                    sub.selections_iter()
                        .flat_map(|selection| selection.names())
                        .unique()
                        .collect()
                } else {
                    Vec::new()
                }
            }
            Self::Group(alias, _) => vec![alias.name.as_str()],
        }
    }

    /// Find the next subselection, if present
    pub(crate) fn next_subselection(&self) -> Option<&SubSelection> {
        match self {
            // Paths are complicated because they can have a subselection deeply nested
            Self::Path { path, .. } => path.next_subselection(),

            // The other options have it at the root
            Self::Field(_, _, Some(sub)) | Self::Group(_, sub) => Some(sub),

            // Every other option does not have a subselection
            _ => None,
        }
    }

    #[allow(unused)]
    pub(crate) fn next_mut_subselection(&mut self) -> Option<&mut SubSelection> {
        match self {
            // Paths are complicated because they can have a subselection deeply nested
            Self::Path { path, .. } => path.next_mut_subselection(),

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
            Self::Path { path, .. } => path.external_var_paths(),
            _ => Vec::new(),
        }
    }
}

// Path                 ::= VarPath | KeyPath | AtPath | ExprPath
// PathSelection        ::= Path SubSelection?
// PathWithSubSelection ::= Path SubSelection
// VarPath              ::= "$" (NO_SPACE Identifier)? PathStep*
// KeyPath              ::= Key PathStep+
// AtPath               ::= "@" PathStep*
// ExprPath             ::= "$(" LitExpr ")" PathStep*
// PathStep             ::= "." Key | "->" Identifier MethodArgs?

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct PathSelection {
    pub(super) path: WithRange<PathList>,
}

// Like NamedSelection, PathSelection is an AST structure that takes its range
// entirely from self.path (a WithRange<PathList>), so PathSelection itself does
// not need to be wrapped as WithRange<PathSelection>, but merely needs to
// implement the Ranged trait.
impl Ranged for PathSelection {
    fn range(&self) -> OffsetRange {
        self.path.range()
    }
}

impl PathSelection {
    pub fn parse(input: Span) -> ParseResult<Self> {
        PathList::parse(input).map(|(input, path)| (input, Self { path }))
    }

    pub(crate) fn variable_reference<N: FromStr + ToString>(&self) -> Option<VariableReference<N>> {
        match self.path.as_ref() {
            PathList::Var(var, tail) => match var.as_ref() {
                KnownVariable::External(namespace) => {
                    let selection = tail.compute_selection_trie();
                    let full_range = merge_ranges(var.range(), tail.range());
                    Some(VariableReference {
                        namespace: VariableNamespace {
                            namespace: N::from_str(namespace).ok()?,
                            location: var.range(),
                        },
                        selection,
                        location: full_range,
                    })
                }
                _ => None,
            },
            _ => None,
        }
    }

    #[allow(unused)]
    pub(super) fn is_single_key(&self) -> bool {
        self.path.is_single_key()
    }

    #[allow(unused)]
    pub(super) fn from_slice(keys: &[Key], selection: Option<SubSelection>) -> Self {
        Self {
            path: WithRange::new(PathList::from_slice(keys, selection), None),
        }
    }

    #[allow(unused)]
    pub(super) fn has_subselection(&self) -> bool {
        self.path.has_subselection()
    }

    pub(super) fn next_subselection(&self) -> Option<&SubSelection> {
        self.path.next_subselection()
    }

    #[allow(unused)]
    pub(super) fn next_mut_subselection(&mut self) -> Option<&mut SubSelection> {
        self.path.next_mut_subselection()
    }
}

impl ExternalVarPaths for PathSelection {
    fn external_var_paths(&self) -> Vec<&PathSelection> {
        let mut paths = Vec::new();
        match self.path.as_ref() {
            PathList::Var(var_name, tail) => {
                if matches!(var_name.as_ref(), KnownVariable::External(_)) {
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
    // variant handles both VarPath and AtPath from the grammar. The
    // PathList::Var variant may only appear at the beginning of a
    // PathSelection's PathList, not in the middle.
    Var(WithRange<KnownVariable>, WithRange<PathList>),

    // A PathSelection that starts with a PathList::Key is a KeyPath, but a
    // PathList::Key also counts as PathStep item, so it may also appear in the
    // middle/tail of a PathList.
    Key(WithRange<Key>, WithRange<PathList>),

    // An ExprPath, which begins with a LitExpr enclosed by $(...). Must appear
    // only at the beginning of a PathSelection, like PathList::Var.
    Expr(WithRange<LitExpr>, WithRange<PathList>),

    // A PathList::Method is a PathStep item that may appear only in the
    // middle/tail (not the beginning) of a PathSelection.
    Method(WithRange<String>, Option<MethodArgs>, WithRange<PathList>),

    // Universal null guard that can wrap any path continuation.
    // If data is null, returns null instead of continuing with the wrapped operation.
    Question(WithRange<PathList>),

    // Optionally, a PathList may end with a SubSelection, which applies a set
    // of named selections to the final value of the path. PathList::Selection
    // by itself is not a valid PathList.
    Selection(SubSelection),

    // Every PathList must be terminated by either PathList::Selection or
    // PathList::Empty. PathList::Empty by itself is not a valid PathList.
    Empty,
}

impl PathList {
    pub(super) fn parse(input: Span) -> ParseResult<WithRange<Self>> {
        match Self::parse_with_depth(input, 0) {
            Ok((_, parsed)) if matches!(*parsed, Self::Empty) => Err(nom_error_message(
                input,
                // As a small technical note, you could consider
                // NamedGroupSelection (an Alias followed by a SubSelection) as
                // a kind of NamedPathSelection where the path is empty, but
                // it's still useful to distinguish groups in the grammar so we
                // can forbid empty paths in general. In fact, when parsing a
                // NamedGroupSelection, this error message is likely to be the
                // reason we abandon parsing NamedPathSelection and correctly
                // fall back to NamedGroupSelection.
                "Path selection cannot be empty",
            )),
            otherwise => otherwise,
        }
    }

    #[cfg(test)]
    pub(super) fn into_with_range(self) -> WithRange<Self> {
        WithRange::new(self, None)
    }

    pub(super) fn parse_with_depth(input: Span, depth: usize) -> ParseResult<WithRange<Self>> {
        // If the input is empty (i.e. this method will end up returning
        // PathList::Empty), we want the OffsetRange to be an empty range at the
        // end of the previously parsed PathList elements, not separated from
        // them by trailing spaces or comments, so we need to capture the empty
        // range before consuming leading spaces_or_comments.
        let offset_if_empty = input.location_offset();
        let range_if_empty: OffsetRange = Some(offset_if_empty..offset_if_empty);

        // Consume leading spaces_or_comments for all cases below.
        let (input, _spaces) = spaces_or_comments(input)?;

        // Variable references (including @ references), $(...) literals, and
        // key references without a leading . are accepted only at depth 0, or
        // at the beginning of the PathSelection.
        if depth == 0 {
            // The $(...) syntax allows embedding LitExpr values within
            // JSONSelection syntax (when not already parsing a LitExpr). This
            // case needs to come before the $ (and $var) case, because $( looks
            // like the $ variable followed by a parse error in the variable
            // case, unless we add some complicated lookahead logic there.
            if let Ok((suffix, (_, dollar_open_paren, expr, close_paren, _))) = tuple((
                spaces_or_comments,
                ranged_span("$("),
                LitExpr::parse,
                spaces_or_comments,
                ranged_span(")"),
            ))(input)
            {
                let (remainder, rest) = Self::parse_with_depth(suffix, depth + 1)?;
                let expr_range = merge_ranges(dollar_open_paren.range(), close_paren.range());
                let full_range = merge_ranges(expr_range, rest.range());
                return Ok((
                    remainder,
                    WithRange::new(Self::Expr(expr, rest), full_range),
                ));
            }

            if let Ok((suffix, (dollar, opt_var))) =
                tuple((ranged_span("$"), opt(parse_identifier_no_space)))(input)
            {
                let dollar_range = dollar.range();
                let (remainder, rest) = Self::parse_with_depth(suffix, depth + 1)?;
                let full_range = merge_ranges(dollar_range.clone(), rest.range());
                return if let Some(var) = opt_var {
                    let full_name = format!("{}{}", dollar.as_ref(), var.as_str());
                    let known_var = KnownVariable::from_str(full_name.as_str());
                    let var_range = merge_ranges(dollar_range, var.range());
                    let ranged_known_var = WithRange::new(known_var, var_range);
                    Ok((
                        remainder,
                        WithRange::new(Self::Var(ranged_known_var, rest), full_range),
                    ))
                } else {
                    let ranged_dollar_var = WithRange::new(KnownVariable::Dollar, dollar_range);
                    Ok((
                        remainder,
                        WithRange::new(Self::Var(ranged_dollar_var, rest), full_range),
                    ))
                };
            }

            if let Ok((suffix, at)) = ranged_span("@")(input) {
                let (remainder, rest) = Self::parse_with_depth(suffix, depth + 1)?;
                let full_range = merge_ranges(at.range(), rest.range());
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
                return match rest.as_ref() {
                    // We use nom_error_message rather than nom_fail_message
                    // here because the key might actually be a field selection,
                    // which means we want to unwind parsing the path and fall
                    // back to parsing other kinds of NamedSelection.
                    Self::Empty | Self::Selection(_) => Err(nom_error_message(
                        input,
                        // Another place where format! might be useful to
                        // suggest .{key}, which would require storing error
                        // messages as owned Strings.
                        "Single-key path must be prefixed with $. to avoid ambiguity with field name",
                    )),
                    _ => {
                        let full_range = merge_ranges(key.range(), rest.range());
                        Ok((remainder, WithRange::new(Self::Key(key, rest), full_range)))
                    }
                };
            }
        }

        if depth == 0 {
            // If the PathSelection does not start with a $var (or $ or @), a
            // key., or $(expr), it is not a valid PathSelection.
            if tuple((ranged_span("."), Key::parse))(input).is_ok() {
                // Since we previously allowed starting key paths with .key but
                // now forbid that syntax (because it can be ambiguous), suggest
                // the unambiguous $.key syntax instead.
                return Err(nom_fail_message(
                    input,
                    "Key paths cannot start with just .key (use $.key instead)",
                ));
            }
            // This error technically covers the case above, but doesn't suggest
            // a helpful solution.
            return Err(nom_error_message(
                input,
                "Path selection must start with key., $variable, $, @, or $(expression)",
            ));
        }

        // Universal optional operator: ? (note: we parse this before other operators to avoid conflicts)
        if let Ok((suffix, question)) = ranged_span("?")(input) {
            // Parse whatever comes after the ? normally
            let (remainder, tail) = Self::parse_with_depth(suffix, depth)?;
            let full_range = merge_ranges(question.range(), tail.range());
            return Ok((remainder, WithRange::new(Self::Question(tail), full_range)));
        }

        // In previous versions of this code, a .key could appear at depth 0 (at
        // the beginning of a path), which was useful to disambiguate a KeyPath
        // consisting of a single key from a field selection.
        //
        // Now that key paths can appear alongside/after named selections within
        // a SubSelection, the .key syntax is potentially unsafe because it may
        // be parsed as a continuation of a previous field selection, since we
        // ignore spaces/newlines/comments between keys in a path.
        //
        // In order to prevent this ambiguity, we now require that a single .key
        // be written as a subproperty of the $ variable, e.g. $.key, which is
        // equivalent to the old behavior, but parses unambiguously. In terms of
        // this code, that means we allow a .key only at depths > 0.
        if let Ok((remainder, (dot, key))) = tuple((ranged_span("."), Key::parse))(input) {
            let (remainder, rest) = Self::parse_with_depth(remainder, depth + 1)?;
            let dot_key_range = merge_ranges(dot.range(), key.range());
            let full_range = merge_ranges(dot_key_range, rest.range());
            return Ok((remainder, WithRange::new(Self::Key(key, rest), full_range)));
        }

        // If we failed to parse "." Key above, but the input starts with a '.'
        // character, it's an error unless it's the beginning of a ... token.
        if input.fragment().starts_with('.') && !input.fragment().starts_with("...") {
            return Err(nom_fail_message(
                input,
                "Path selection . must be followed by key (identifier or quoted string literal)",
            ));
        }

        // PathSelection can never start with a naked ->method (instead, use
        // $->method or @->method if you want to operate on the current value).
        if let Ok((suffix, arrow)) = ranged_span("->")(input) {
            // As soon as we see a -> token, we know what follows must be a
            // method name, so we can unconditionally return based on what
            // parse_identifier tells us. since MethodArgs::parse is optional,
            // the absence of args will never trigger the error case.
            return match tuple((parse_identifier, opt(MethodArgs::parse)))(suffix) {
                Ok((suffix, (method, args))) => {
                    let (remainder, rest) = Self::parse_with_depth(suffix, depth + 1)?;
                    let full_range = merge_ranges(arrow.range(), rest.range());
                    Ok((
                        remainder,
                        WithRange::new(Self::Method(method, args, rest), full_range),
                    ))
                }
                Err(_) => Err(nom_fail_message(input, "Method name must follow ->")),
            };
        }

        // Likewise, if the PathSelection has a SubSelection, it must appear at
        // the end of a non-empty path. PathList::parse_with_depth is not
        // responsible for enforcing a trailing SubSelection in the
        // PathWithSubSelection case, since that requirement is checked by
        // NamedSelection::parse_path.
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

    #[allow(unused)]
    pub(super) fn from_slice(properties: &[Key], selection: Option<SubSelection>) -> Self {
        match properties {
            [] => selection.map_or(Self::Empty, Self::Selection),
            [head, tail @ ..] => Self::Key(
                WithRange::new(head.clone(), None),
                WithRange::new(Self::from_slice(tail, selection), None),
            ),
        }
    }

    pub(super) fn has_subselection(&self) -> bool {
        self.next_subselection().is_some()
    }

    /// Find the next subselection, traversing nested chains if needed
    pub(super) fn next_subselection(&self) -> Option<&SubSelection> {
        match self {
            Self::Var(_, tail) => tail.next_subselection(),
            Self::Key(_, tail) => tail.next_subselection(),
            Self::Expr(_, tail) => tail.next_subselection(),
            Self::Method(_, _, tail) => tail.next_subselection(),
            Self::Question(tail) => tail.next_subselection(),
            Self::Selection(sub) => Some(sub),
            Self::Empty => None,
        }
    }

    #[allow(unused)]
    /// Find the next subselection, traversing nested chains if needed. Returns a mutable reference
    pub(super) fn next_mut_subselection(&mut self) -> Option<&mut SubSelection> {
        match self {
            Self::Var(_, tail) => tail.next_mut_subselection(),
            Self::Key(_, tail) => tail.next_mut_subselection(),
            Self::Expr(_, tail) => tail.next_mut_subselection(),
            Self::Method(_, _, tail) => tail.next_mut_subselection(),
            Self::Question(tail) => tail.next_mut_subselection(),
            Self::Selection(sub) => Some(sub),
            Self::Empty => None,
        }
    }
}

impl ExternalVarPaths for PathList {
    fn external_var_paths(&self) -> Vec<&PathSelection> {
        let mut paths = Vec::new();
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
            PathList::Expr(expr, rest) => {
                paths.extend(expr.external_var_paths());
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
            PathList::Question(rest) => {
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
    pub(super) range: OffsetRange,
}

impl Ranged for SubSelection {
    // Since SubSelection is a struct, we can store its range directly as a
    // field of the struct, allowing SubSelection to implement the Ranged trait
    // without a WithRange<SubSelection> wrapper.
    fn range(&self) -> OffsetRange {
        self.range.clone()
    }
}

impl SubSelection {
    pub(crate) fn parse(input: Span) -> ParseResult<Self> {
        match tuple((
            spaces_or_comments,
            ranged_span("{"),
            Self::parse_naked,
            spaces_or_comments,
            ranged_span("}"),
        ))(input)
        {
            Ok((remainder, (_, open_brace, sub, _, close_brace))) => {
                let range = merge_ranges(open_brace.range(), close_brace.range());
                Ok((
                    remainder,
                    Self {
                        selections: sub.selections,
                        range,
                    },
                ))
            }
            Err(e) => Err(e),
        }
    }

    fn parse_naked(input: Span) -> ParseResult<Self> {
        many0(NamedSelection::parse)(input).map(|(remainder, selections)| {
            let range = merge_ranges(
                selections.first().and_then(|first| first.range()),
                selections.last().and_then(|last| last.range()),
            );

            (remainder, Self { selections, range })
        })
    }

    // Returns an Iterator over each &NamedSelection that contributes a single
    // name to the output object. This is more complicated than returning
    // self.selections.iter() because some NamedSelection::Path elements can
    // contribute multiple names if they do no have an Alias.
    pub fn selections_iter(&self) -> impl Iterator<Item = &NamedSelection> {
        // TODO Implement a NamedSelectionIterator to traverse nested selections
        // lazily, rather than using an intermediary vector.
        let mut selections = Vec::new();
        for selection in &self.selections {
            match selection {
                NamedSelection::Path { alias, path, .. } => {
                    if alias.is_some() {
                        // If the PathSelection has an Alias, then it has a
                        // singular name and should be visited directly.
                        selections.push(selection);
                    } else if let Some(sub) = path.next_subselection() {
                        // If the PathSelection does not have an Alias but does
                        // have a SubSelection, then it represents the
                        // PathWithSubSelection non-terminal from the grammar
                        // (see README.md + PR #6076), which produces multiple
                        // names derived from the SubSelection, which need to be
                        // recursively collected.
                        selections.extend(sub.selections_iter());
                    } else {
                        // This no-Alias, no-SubSelection case should be
                        // forbidden by NamedSelection::parse_path.
                        debug_assert!(false, "PathSelection without Alias or SubSelection");
                    }
                }
                _ => {
                    selections.push(selection);
                }
            };
        }
        selections.into_iter()
    }

    pub fn append_selection(&mut self, selection: NamedSelection) {
        self.selections.push(selection);
    }

    pub fn last_selection_mut(&mut self) -> Option<&mut NamedSelection> {
        self.selections.last_mut()
    }
}

impl ExternalVarPaths for SubSelection {
    fn external_var_paths(&self) -> Vec<&PathSelection> {
        let mut paths = Vec::new();
        for selection in &self.selections {
            paths.extend(selection.external_var_paths());
        }
        paths
    }
}

// Alias ::= Key ":"

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct Alias {
    pub(super) name: WithRange<Key>,
    pub(super) range: OffsetRange,
}

impl Ranged for Alias {
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

    fn parse(input: Span) -> ParseResult<Self> {
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
    pub fn parse(input: Span) -> ParseResult<WithRange<Self>> {
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

pub(super) fn is_identifier(input: &str) -> bool {
    all_consuming(parse_identifier_no_space)(new_span(input)).is_ok()
}

fn parse_identifier(input: Span) -> ParseResult<WithRange<String>> {
    preceded(spaces_or_comments, parse_identifier_no_space)(input)
}

fn parse_identifier_no_space(input: Span) -> ParseResult<WithRange<String>> {
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

pub(crate) fn parse_string_literal(input: Span) -> ParseResult<WithRange<String>> {
    let input = spaces_or_comments(input)?.0;
    let start = input.location_offset();
    let mut input_char_indices = input.char_indices();

    match input_char_indices.next() {
        Some((0, quote @ '\'')) | Some((0, quote @ '"')) => {
            let mut escape_next = false;
            let mut chars: Vec<char> = Vec::new();
            let mut remainder_opt: Option<Span> = None;

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
                    remainder_opt = Some(input.slice(i + 1..));
                    break;
                }
                chars.push(c);
            }

            remainder_opt
                .ok_or_else(|| nom_fail_message(input, "Unterminated string literal"))
                .map(|remainder| {
                    (
                        remainder,
                        WithRange::new(
                            chars.iter().collect::<String>(),
                            Some(start..remainder.location_offset()),
                        ),
                    )
                })
        }

        _ => Err(nom_error_message(input, "Not a string literal")),
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Default)]
pub(super) struct MethodArgs {
    pub(super) args: Vec<WithRange<LitExpr>>,
    pub(super) range: OffsetRange,
}

impl Ranged for MethodArgs {
    fn range(&self) -> OffsetRange {
        self.range.clone()
    }
}

// Comma-separated positional arguments for a method, surrounded by parentheses.
// When an arrow method is used without arguments, the Option<MethodArgs> for
// the PathSelection::Method will be None, so we can safely define MethodArgs
// using a Vec<LitExpr> in all cases (possibly empty but never missing).
impl MethodArgs {
    fn parse(input: Span) -> ParseResult<Self> {
        let input = spaces_or_comments(input)?.0;
        let (mut input, open_paren) = ranged_span("(")(input)?;
        input = spaces_or_comments(input)?.0;

        let mut args = Vec::new();
        if let Ok((remainder, first)) = LitExpr::parse(input) {
            args.push(first);
            input = remainder;

            while let Ok((remainder, _)) = tuple((spaces_or_comments, char(',')))(input) {
                input = spaces_or_comments(remainder)?.0;
                if let Ok((remainder, arg)) = LitExpr::parse(input) {
                    args.push(arg);
                    input = remainder;
                } else {
                    break;
                }
            }
        }

        input = spaces_or_comments(input)?.0;
        let (input, close_paren) = ranged_span(")")(input)?;

        let range = merge_ranges(open_paren.range(), close_paren.range());
        Ok((input, Self { args, range }))
    }
}

#[cfg(test)]
mod tests {
    use apollo_compiler::collections::IndexMap;

    use super::super::location::strip_ranges::StripRanges;
    use super::*;
    use crate::assert_debug_snapshot;
    use crate::connectors::json_selection::PrettyPrintable;
    use crate::connectors::json_selection::SelectionTrie;
    use crate::connectors::json_selection::fixtures::Namespace;
    use crate::connectors::json_selection::helpers::span_is_all_spaces_or_comments;
    use crate::connectors::json_selection::location::new_span;
    use crate::selection;

    #[test]
    fn test_identifier() {
        fn check(input: &str, expected_name: &str) {
            let (remainder, name) = parse_identifier(new_span(input)).unwrap();
            assert!(
                span_is_all_spaces_or_comments(remainder),
                "remainder is `{remainder}`"
            );
            assert_eq!(name.as_ref(), expected_name);
        }

        check("hello", "hello");
        check("hello_world", "hello_world");
        check("  hello_world ", "hello_world");
        check("hello_world_123", "hello_world_123");
        check(" hello ", "hello");

        fn check_no_space(input: &str, expected_name: &str) {
            let name = parse_identifier_no_space(new_span(input)).unwrap().1;
            assert_eq!(name.as_ref(), expected_name);
        }

        check_no_space("oyez", "oyez");
        check_no_space("oyez   ", "oyez");

        {
            let identifier_with_leading_space = new_span("  oyez   ");
            assert_eq!(
                parse_identifier_no_space(identifier_with_leading_space),
                Err(nom::Err::Error(nom::error::Error::from_error_kind(
                    // The parse_identifier_no_space function does not provide a
                    // custom error message, since it's only used internally.
                    // Testing it directly here is somewhat contrived.
                    identifier_with_leading_space,
                    nom::error::ErrorKind::OneOf,
                ))),
            );
        }
    }

    #[test]
    fn test_string_literal() {
        fn check(input: &str, expected: &str) {
            let (remainder, lit) = parse_string_literal(new_span(input)).unwrap();
            assert!(
                span_is_all_spaces_or_comments(remainder),
                "remainder is `{remainder}`"
            );
            assert_eq!(lit.as_ref(), expected);
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
            let (remainder, key) = Key::parse(new_span(input)).unwrap();
            assert!(
                span_is_all_spaces_or_comments(remainder),
                "remainder is `{remainder}`"
            );
            assert_eq!(key.as_ref(), expected);
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
            let (remainder, parsed) = Alias::parse(new_span(input)).unwrap();
            assert!(
                span_is_all_spaces_or_comments(remainder),
                "remainder is `{remainder}`"
            );
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
        fn assert_result_and_names(input: &str, expected: NamedSelection, names: &[&str]) {
            let (remainder, selection) = NamedSelection::parse(new_span(input)).unwrap();
            assert!(
                span_is_all_spaces_or_comments(remainder),
                "remainder is `{remainder}`"
            );
            let selection = selection.strip_ranges();
            assert_eq!(selection, expected);
            assert_eq!(selection.names(), names);
            assert_eq!(
                selection!(input).strip_ranges(),
                JSONSelection::Named(SubSelection {
                    selections: vec![expected],
                    ..Default::default()
                },),
            );
        }

        assert_result_and_names(
            "hello",
            NamedSelection::Field(None, Key::field("hello").into_with_range(), None),
            &["hello"],
        );

        assert_result_and_names(
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
            &["hello"],
        );

        assert_result_and_names(
            "hi: hello",
            NamedSelection::Field(
                Some(Alias::new("hi")),
                Key::field("hello").into_with_range(),
                None,
            ),
            &["hi"],
        );

        assert_result_and_names(
            "hi: 'hello world'",
            NamedSelection::Field(
                Some(Alias::new("hi")),
                Key::quoted("hello world").into_with_range(),
                None,
            ),
            &["hi"],
        );

        assert_result_and_names(
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
            &["hi"],
        );

        assert_result_and_names(
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
            &["hey"],
        );

        assert_result_and_names(
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
            &["hey"],
        );

        assert_result_and_names(
            "leggo: 'my ego'",
            NamedSelection::Field(
                Some(Alias::new("leggo")),
                Key::quoted("my ego").into_with_range(),
                None,
            ),
            &["leggo"],
        );

        assert_result_and_names(
            "'let go': 'my ego'",
            NamedSelection::Field(
                Some(Alias::quoted("let go")),
                Key::quoted("my ego").into_with_range(),
                None,
            ),
            &["let go"],
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
            selection!("$.hello").strip_ranges(),
            JSONSelection::Path(PathSelection {
                path: PathList::Var(
                    KnownVariable::Dollar.into_with_range(),
                    PathList::Key(
                        Key::field("hello").into_with_range(),
                        PathList::Empty.into_with_range()
                    )
                    .into_with_range(),
                )
                .into_with_range(),
            }),
        );

        {
            let expected = JSONSelection::Named(SubSelection {
                selections: vec![NamedSelection::Path {
                    alias: Some(Alias::new("hi")),
                    inline: false,
                    path: PathSelection::from_slice(
                        &[
                            Key::Field("hello".to_string()),
                            Key::Field("world".to_string()),
                        ],
                        None,
                    ),
                }],
                ..Default::default()
            });

            assert_eq!(selection!("hi: hello.world").strip_ranges(), expected);
            assert_eq!(selection!("hi: hello .world").strip_ranges(), expected);
            assert_eq!(selection!("hi:  hello. world").strip_ranges(), expected);
            assert_eq!(selection!("hi: hello . world").strip_ranges(), expected);
            assert_eq!(selection!("hi: hello.world").strip_ranges(), expected);
            assert_eq!(selection!("hi: hello. world").strip_ranges(), expected);
            assert_eq!(selection!("hi: hello .world").strip_ranges(), expected);
            assert_eq!(selection!("hi: hello . world ").strip_ranges(), expected);
        }

        {
            let expected = JSONSelection::Named(SubSelection {
                selections: vec![
                    NamedSelection::Field(None, Key::field("before").into_with_range(), None),
                    NamedSelection::Path {
                        alias: Some(Alias::new("hi")),
                        inline: false,
                        path: PathSelection::from_slice(
                            &[
                                Key::Field("hello".to_string()),
                                Key::Field("world".to_string()),
                            ],
                            None,
                        ),
                    },
                    NamedSelection::Field(None, Key::field("after").into_with_range(), None),
                ],
                ..Default::default()
            });

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
            assert_eq!(
                selection!("before hi:  hello.world after").strip_ranges(),
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
                    NamedSelection::Path {
                        alias: Some(Alias::new("hi")),
                        inline: false,
                        path: PathSelection::from_slice(
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
                    },
                    NamedSelection::Field(None, Key::field("after").into_with_range(), None),
                ],
                ..Default::default()
            });

            assert_eq!(
                selection!("before hi: hello.world { nested names } after").strip_ranges(),
                expected
            );
            assert_eq!(
                selection!("before hi:hello.world{nested names}after").strip_ranges(),
                expected
            );
            assert_eq!(
                selection!(" before hi : hello . world { nested names } after ").strip_ranges(),
                expected
            );
        }

        assert_debug_snapshot!(selection!(
            "
            # Comments are supported because we parse them as whitespace
            topLevelAlias: topLevelField {
                identifier: 'property name with spaces'
                'unaliased non-identifier property'
                'non-identifier alias': identifier

                # This extracts the value located at the given path and applies a
                # selection set to it before renaming the result to pathSelection
                pathSelection: some.nested.path {
                    still: yet
                    more
                    properties
                }

                # An aliased SubSelection of fields nests the fields together
                # under the given alias
                siblingGroup: { brother sister }
            }"
        ));
    }

    #[track_caller]
    fn check_path_selection(input: &str, expected: PathSelection) {
        let (remainder, path_selection) = PathSelection::parse(new_span(input)).unwrap();
        assert!(
            span_is_all_spaces_or_comments(remainder),
            "remainder is `{remainder}`"
        );
        assert_eq!(&path_selection.strip_ranges(), &expected);
        assert_eq!(
            selection!(input).strip_ranges(),
            JSONSelection::Path(expected)
        );
    }

    #[test]
    fn test_path_selection() {
        check_path_selection(
            "$.hello",
            PathSelection {
                path: PathList::Var(
                    KnownVariable::Dollar.into_with_range(),
                    PathList::Key(
                        Key::field("hello").into_with_range(),
                        PathList::Empty.into_with_range(),
                    )
                    .into_with_range(),
                )
                .into_with_range(),
            },
        );

        {
            let expected = PathSelection {
                path: PathList::Var(
                    KnownVariable::Dollar.into_with_range(),
                    PathList::Key(
                        Key::field("hello").into_with_range(),
                        PathList::Key(
                            Key::field("world").into_with_range(),
                            PathList::Empty.into_with_range(),
                        )
                        .into_with_range(),
                    )
                    .into_with_range(),
                )
                .into_with_range(),
            };
            check_path_selection("$.hello.world", expected.clone());
            check_path_selection("$.hello .world", expected.clone());
            check_path_selection("$.hello. world", expected.clone());
            check_path_selection("$.hello . world", expected.clone());
            check_path_selection("$ . hello . world", expected.clone());
            check_path_selection(" $ . hello . world ", expected);
        }

        {
            let expected = PathSelection::from_slice(
                &[
                    Key::Field("hello".to_string()),
                    Key::Field("world".to_string()),
                ],
                None,
            );
            check_path_selection("hello.world", expected.clone());
            check_path_selection("hello .world", expected.clone());
            check_path_selection("hello. world", expected.clone());
            check_path_selection("hello . world", expected.clone());
            check_path_selection(" hello . world ", expected);
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
            check_path_selection("hello.world{hello}", expected.clone());
            check_path_selection("hello.world { hello }", expected.clone());
            check_path_selection("hello .world { hello }", expected.clone());
            check_path_selection("hello. world { hello }", expected.clone());
            check_path_selection("hello . world { hello }", expected.clone());
            check_path_selection(" hello . world { hello } ", expected);
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
            check_path_selection(
                " nested . 'string literal' . \"property\" . name ",
                expected,
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
                "nested.'string literal' { leggo: 'my ego' }",
                expected.clone(),
            );

            check_path_selection(
                " nested . 'string literal' { leggo : 'my ego' } ",
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
            check_path_selection(
                " nested . \"string literal\" { leggo: 'my ego' } ",
                expected,
            );
        }

        {
            let expected = PathSelection {
                path: PathList::Var(
                    KnownVariable::Dollar.into_with_range(),
                    PathList::Key(
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
                )
                .into_with_range(),
            };
            check_path_selection(
                "$.results{'quoted without alias'{id'n a m e'}}",
                expected.clone(),
            );
            check_path_selection(
                " $ . results { 'quoted without alias' { id 'n a m e' } } ",
                expected,
            );
        }

        {
            let expected = PathSelection {
                path: PathList::Var(
                    KnownVariable::Dollar.into_with_range(),
                    PathList::Key(
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
                )
                .into_with_range(),
            };
            check_path_selection(
                "$.results{'non-identifier alias':'quoted with alias'{id'n a m e':name}}",
                expected.clone(),
            );
            check_path_selection(
                " $ . results { 'non-identifier alias' : 'quoted with alias' { id 'n a m e': name } } ",
                expected,
            );
        }
    }

    #[test]
    fn test_path_selection_vars() {
        check_path_selection(
            "$this",
            PathSelection {
                path: PathList::Var(
                    KnownVariable::External(Namespace::This.to_string()).into_with_range(),
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
                    KnownVariable::External(Namespace::This.to_string()).into_with_range(),
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
                KnownVariable::External(Namespace::This.to_string()).into_with_range(),
                PathList::Selection(SubSelection {
                    selections: vec![
                        NamedSelection::Field(None, Key::field("before").into_with_range(), None),
                        NamedSelection::Path {
                            alias: Some(Alias::new("alias")),
                            inline: false,
                            path: PathSelection {
                                path: PathList::Var(
                                    KnownVariable::External(Namespace::Args.to_string())
                                        .into_with_range(),
                                    PathList::Key(
                                        Key::field("arg").into_with_range(),
                                        PathList::Empty.into_with_range(),
                                    )
                                    .into_with_range(),
                                )
                                .into_with_range(),
                            },
                        },
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
                                NamedSelection::Path {
                                    alias: Some(Alias::new("injected")),
                                    inline: false,
                                    path: PathSelection {
                                        path: PathList::Var(
                                            KnownVariable::External(Namespace::Args.to_string())
                                                .into_with_range(),
                                            PathList::Key(
                                                Key::field("arg").into_with_range(),
                                                PathList::Empty.into_with_range(),
                                            )
                                            .into_with_range(),
                                        )
                                        .into_with_range(),
                                    },
                                },
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
                    KnownVariable::External(Namespace::Args.to_string()).into_with_range(),
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
            "root.x.y.z",
            PathSelection::from_slice(
                &[
                    Key::Field("root".to_string()),
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

        #[track_caller]
        fn check_path_parse_error(input: &str, expected_offset: usize, expected_message: &str) {
            match PathSelection::parse(new_span(input)) {
                Ok((remainder, path)) => {
                    panic!(
                        "Expected error at offset {} with message '{}', but got path {:?} and remainder {:?}",
                        expected_offset, expected_message, path, remainder,
                    );
                }
                Err(nom::Err::Error(e) | nom::Err::Failure(e)) => {
                    assert_eq!(&input[expected_offset..], *e.input.fragment());
                    // The PartialEq implementation for LocatedSpan
                    // unfortunately ignores span.extra, so we have to check
                    // e.input.extra manually.
                    assert_eq!(e.input.extra, Some(expected_message));
                }
                Err(e) => {
                    panic!("Unexpected error {:?}", e);
                }
            }
        }

        let single_key_path_error_message =
            "Single-key path must be prefixed with $. to avoid ambiguity with field name";
        check_path_parse_error(
            new_span("naked").fragment(),
            0,
            single_key_path_error_message,
        );
        check_path_parse_error(
            new_span("naked { hi }").fragment(),
            0,
            single_key_path_error_message,
        );
        check_path_parse_error(
            new_span("  naked { hi }").fragment(),
            2,
            single_key_path_error_message,
        );

        let path_key_ambiguity_error_message =
            "Path selection . must be followed by key (identifier or quoted string literal)";
        check_path_parse_error(
            new_span("valid.$invalid").fragment(),
            5,
            path_key_ambiguity_error_message,
        );
        check_path_parse_error(
            new_span("  valid.$invalid").fragment(),
            7,
            path_key_ambiguity_error_message,
        );
        check_path_parse_error(
            new_span("  valid . $invalid").fragment(),
            8,
            path_key_ambiguity_error_message,
        );

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
                    KnownVariable::External(Namespace::This.to_string()).into_with_range(),
                    PathList::Empty.into_with_range()
                )
                .into_with_range(),
            }),
        );

        assert_eq!(
            selection!("value: $ a { b c }").strip_ranges(),
            JSONSelection::Named(SubSelection {
                selections: vec![
                    NamedSelection::Path {
                        alias: Some(Alias::new("value")),
                        inline: false,
                        path: PathSelection {
                            path: PathList::Var(
                                KnownVariable::Dollar.into_with_range(),
                                PathList::Empty.into_with_range()
                            )
                            .into_with_range(),
                        },
                    },
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
                selections: vec![NamedSelection::Path {
                    alias: Some(Alias::new("value")),
                    inline: false,
                    path: PathSelection {
                        path: PathList::Var(
                            KnownVariable::External(Namespace::This.to_string()).into_with_range(),
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
                }],
                ..Default::default()
            }),
        );
    }

    #[test]
    fn test_error_snapshots() {
        // The .data shorthand is no longer allowed, since it can be mistakenly
        // parsed as a continuation of a previous selection. Instead, use $.data
        // to achieve the same effect without ambiguity.
        assert_debug_snapshot!(JSONSelection::parse(".data"));

        // If you want to mix a path selection with other named selections, the
        // path selection must have a trailing subselection, to enforce that it
        // returns an object with statically known keys.
        assert_debug_snapshot!(JSONSelection::parse("id $.object"));
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
    fn test_expr_path_selections() {
        fn check_simple_lit_expr(input: &str, expected: LitExpr) {
            check_path_selection(
                input,
                PathSelection {
                    path: PathList::Expr(
                        expected.into_with_range(),
                        PathList::Empty.into_with_range(),
                    )
                    .into_with_range(),
                },
            );
        }

        check_simple_lit_expr("$(null)", LitExpr::Null);

        check_simple_lit_expr("$(true)", LitExpr::Bool(true));
        check_simple_lit_expr("$(false)", LitExpr::Bool(false));

        check_simple_lit_expr(
            "$(1234)",
            LitExpr::Number("1234".parse().expect("serde_json::Number parse error")),
        );
        check_simple_lit_expr(
            "$(1234.5678)",
            LitExpr::Number("1234.5678".parse().expect("serde_json::Number parse error")),
        );

        check_simple_lit_expr(
            "$('hello world')",
            LitExpr::String("hello world".to_string()),
        );
        check_simple_lit_expr(
            "$(\"hello world\")",
            LitExpr::String("hello world".to_string()),
        );
        check_simple_lit_expr(
            "$(\"hello \\\"world\\\"\")",
            LitExpr::String("hello \"world\"".to_string()),
        );

        check_simple_lit_expr(
            "$([1, 2, 3])",
            LitExpr::Array(
                vec!["1".parse(), "2".parse(), "3".parse()]
                    .into_iter()
                    .map(|n| {
                        LitExpr::Number(n.expect("serde_json::Number parse error"))
                            .into_with_range()
                    })
                    .collect(),
            ),
        );

        check_simple_lit_expr("$({})", LitExpr::Object(IndexMap::default()));
        check_simple_lit_expr(
            "$({ a: 1, b: 2, c: 3 })",
            LitExpr::Object({
                let mut map = IndexMap::default();
                for (key, value) in &[("a", "1"), ("b", "2"), ("c", "3")] {
                    map.insert(
                        Key::field(key).into_with_range(),
                        LitExpr::Number(value.parse().expect("serde_json::Number parse error"))
                            .into_with_range(),
                    );
                }
                map
            }),
        );

        assert_debug_snapshot!(
            // Using extra spaces here to make sure the ranges don't
            // accidentally include leading/trailing spaces.
            selection!(" suffix : results -> slice ( $( - 1 ) -> mul ( $args . suffixLength ) ) ")
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
                                args: vec![
                                    LitExpr::Path(PathSelection::from_slice(
                                        &[Key::field("data"), Key::field("y")],
                                        None,
                                    ))
                                    .into_with_range(),
                                ],
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
            fn make_dollar_key_expr(key: &str) -> WithRange<LitExpr> {
                WithRange::new(
                    LitExpr::Path(PathSelection {
                        path: PathList::Var(
                            KnownVariable::Dollar.into_with_range(),
                            PathList::Key(
                                Key::field(key).into_with_range(),
                                PathList::Empty.into_with_range(),
                            )
                            .into_with_range(),
                        )
                        .into_with_range(),
                    }),
                    None,
                )
            }

            let expected = PathSelection {
                path: PathList::Key(
                    Key::field("data").into_with_range(),
                    PathList::Method(
                        WithRange::new("query".to_string(), None),
                        Some(MethodArgs {
                            args: vec![
                                make_dollar_key_expr("a"),
                                make_dollar_key_expr("b"),
                                make_dollar_key_expr("c"),
                            ],
                            ..Default::default()
                        }),
                        PathList::Empty.into_with_range(),
                    )
                    .into_with_range(),
                )
                .into_with_range(),
            };
            check_path_selection("data->query($.a, $.b, $.c)", expected.clone());
            check_path_selection("data->query($.a, $.b, $.c )", expected.clone());
            check_path_selection("data->query($.a, $.b, $.c,)", expected.clone());
            check_path_selection("data->query($.a, $.b, $.c ,)", expected.clone());
            check_path_selection("data->query($.a, $.b, $.c , )", expected);
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
                                args: vec![
                                    LitExpr::Array(vec![
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
                                    .into_with_range(),
                                ],
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
            check_path_selection("data.x->concat([data.y, data.z , ] , )", expected);
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
                                                selections: vec![NamedSelection::Path {
                                                    alias: Some(Alias::new("x2")),
                                                    inline: false,
                                                    path: PathSelection {
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
                                                }],
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
                                                selections: vec![NamedSelection::Path {
                                                    alias: Some(Alias::new("y2")),
                                                    inline: false,
                                                    path: PathSelection {
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
                                                }],
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
    fn test_path_with_subselection() {
        assert_debug_snapshot!(selection!(
            r#"
            choices->first.message { content role }
        "#
        ));

        assert_debug_snapshot!(selection!(
            r#"
            id
            created
            choices->first.message { content role }
            model
        "#
        ));

        assert_debug_snapshot!(selection!(
            r#"
            id
            created
            choices->first.message { content role }
            model
            choices->last.message { lastContent: content }
        "#
        ));

        assert_debug_snapshot!(JSONSelection::parse(
            r#"
            id
            created
            choices->first.message
            model
        "#
        ));

        assert_debug_snapshot!(JSONSelection::parse(
            r#"
            id: $this.id
            $args.input {
                title
                body
            }
        "#
        ));

        // Like the selection above, this selection produces an output shape
        // with id, title, and body all flattened in a top-level object.
        assert_debug_snapshot!(JSONSelection::parse(
            r#"
            $this { id }
            $args { $.input { title body } }
        "#
        ));

        assert_debug_snapshot!(JSONSelection::parse(
            r#"
            # Equivalent to id: $this.id
            $this { id }

            $args {
                __typename: $("Args")

                # Using $. instead of just . prevents .input from
                # parsing as a key applied to the $("Args") string.
                $.input { title body }

                extra
            }

            from: $.from
        "#
        ));
    }

    #[test]
    fn test_subselection() {
        fn check_parsed(input: &str, expected: SubSelection) {
            let (remainder, parsed) = SubSelection::parse(new_span(input)).unwrap();
            assert!(
                span_is_all_spaces_or_comments(remainder),
                "remainder is `{remainder}`"
            );
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
    fn test_external_var_paths() {
        fn parse(input: &str) -> PathSelection {
            PathSelection::parse(new_span(input))
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
            let parsed = JSONSelection::parse(input).unwrap();
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
                    }),
                )],
                range: Some(2..20),
            }),
        );

        check(
            "$args.product.id",
            JSONSelection::Path(PathSelection {
                path: WithRange::new(
                    PathList::Var(
                        WithRange::new(
                            KnownVariable::External(Namespace::Args.to_string()),
                            Some(0..5),
                        ),
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
                        WithRange::new(
                            KnownVariable::External(Namespace::Args.to_string()),
                            Some(1..6),
                        ),
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
                    NamedSelection::Path {
                        alias: Some(Alias {
                            name: WithRange::new(Key::field("product"), Some(7..14)),
                            range: Some(7..15),
                        }),
                        inline: false,
                        path: PathSelection {
                            path: WithRange::new(
                                PathList::Var(
                                    WithRange::new(
                                        KnownVariable::External(Namespace::Args.to_string()),
                                        Some(15..20),
                                    ),
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
                    },
                    NamedSelection::Field(
                        None,
                        WithRange::new(Key::field("after"), Some(37..42)),
                        None,
                    ),
                ],
                range: Some(0..42),
            }),
        );
    }

    #[test]
    fn test_variable_reference_no_path() {
        let selection = JSONSelection::parse("$this").unwrap();
        let var_paths = selection.external_var_paths();
        assert_eq!(var_paths.len(), 1);
        assert_eq!(
            var_paths[0].variable_reference(),
            Some(VariableReference {
                namespace: VariableNamespace {
                    namespace: Namespace::This,
                    location: Some(0..5),
                },
                selection: {
                    let mut selection = SelectionTrie::new();
                    selection.add_str_path([]);
                    selection
                },
                location: Some(0..5),
            })
        );
    }

    #[test]
    fn test_variable_reference_with_path() {
        let selection = JSONSelection::parse("$this.a.b.c").unwrap();
        let var_paths = selection.external_var_paths();
        assert_eq!(var_paths.len(), 1);

        let var_ref = var_paths[0].variable_reference().unwrap();
        assert_eq!(
            var_ref.namespace,
            VariableNamespace {
                namespace: Namespace::This,
                location: Some(0..5)
            }
        );
        assert_eq!(var_ref.selection.to_string(), "a { b { c } }");
        assert_eq!(var_ref.location, Some(0..11));

        assert_eq!(
            var_ref.selection.key_ranges("a").collect::<Vec<_>>(),
            vec![6..7]
        );
        let a_trie = var_ref.selection.get("a").unwrap();
        assert_eq!(a_trie.key_ranges("b").collect::<Vec<_>>(), vec![8..9]);
        let b_trie = a_trie.get("b").unwrap();
        assert_eq!(b_trie.key_ranges("c").collect::<Vec<_>>(), vec![10..11]);
    }

    #[test]
    fn test_variable_reference_nested() {
        let selection = JSONSelection::parse("a b { c: $this.x.y.z { d } }").unwrap();
        let var_paths = selection.external_var_paths();
        assert_eq!(var_paths.len(), 1);

        let var_ref = var_paths[0].variable_reference().unwrap();
        assert_eq!(
            var_ref.namespace,
            VariableNamespace {
                namespace: Namespace::This,
                location: Some(9..14),
            }
        );
        assert_eq!(var_ref.selection.to_string(), "x { y { z { d } } }");
        assert_eq!(var_ref.location, Some(9..26));

        assert_eq!(
            var_ref.selection.key_ranges("x").collect::<Vec<_>>(),
            vec![15..16]
        );
        let x_trie = var_ref.selection.get("x").unwrap();
        assert_eq!(x_trie.key_ranges("y").collect::<Vec<_>>(), vec![17..18]);
        let y_trie = x_trie.get("y").unwrap();
        assert_eq!(y_trie.key_ranges("z").collect::<Vec<_>>(), vec![19..20]);
        let z_trie = y_trie.get("z").unwrap();
        assert_eq!(z_trie.key_ranges("d").collect::<Vec<_>>(), vec![23..24]);
    }

    #[test]
    fn test_external_var_paths_no_variable() {
        let selection = JSONSelection::parse("a.b.c").unwrap();
        let var_paths = selection.external_var_paths();
        assert_eq!(var_paths.len(), 0);
    }

    #[test]
    fn test_naked_literal_path_for_connect_v0_2() {
        let selection_null_stringify_v0_2 = JSONSelection::parse("$(null->jsonStringify)").unwrap();
        assert_eq!(
            selection_null_stringify_v0_2.pretty_print(),
            "$(null->jsonStringify)"
        );

        let selection_hello_slice_v0_2 =
            JSONSelection::parse("sliced: $('hello'->slice(1, 3))").unwrap();
        assert_eq!(
            selection_hello_slice_v0_2.pretty_print(),
            "sliced: $(\"hello\"->slice(1, 3))"
        );

        let selection_true_not_v0_2 = JSONSelection::parse("true->not").unwrap();
        assert_eq!(selection_true_not_v0_2.pretty_print(), "true->not");

        let selection_false_not_v0_2 = JSONSelection::parse("false->not").unwrap();
        assert_eq!(selection_false_not_v0_2.pretty_print(), "false->not");

        let selection_object_path_v0_2 = JSONSelection::parse("$({ a: 123 }.a)").unwrap();
        assert_eq!(
            selection_object_path_v0_2.pretty_print_with_indentation(true, 0),
            "$({ a: 123 }.a)"
        );

        let selection_array_path_v0_2 = JSONSelection::parse("$([1, 2, 3]->get(1))").unwrap();
        assert_eq!(
            selection_array_path_v0_2.pretty_print(),
            "$([1, 2, 3]->get(1))"
        );

        assert_debug_snapshot!(selection_null_stringify_v0_2);
        assert_debug_snapshot!(selection_hello_slice_v0_2);
        assert_debug_snapshot!(selection_true_not_v0_2);
        assert_debug_snapshot!(selection_false_not_v0_2);
        assert_debug_snapshot!(selection_object_path_v0_2);
        assert_debug_snapshot!(selection_array_path_v0_2);
    }

    #[test]
    fn test_optional_key_access() {
        // Test optional key access: foo?.bar
        check_path_selection(
            "$.foo?.bar",
            PathSelection {
                path: PathList::Var(
                    KnownVariable::Dollar.into_with_range(),
                    PathList::Key(
                        Key::field("foo").into_with_range(),
                        PathList::Question(
                            PathList::Key(
                                Key::field("bar").into_with_range(),
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
    }

    #[test]
    fn test_optional_method_call() {
        // Test optional method call: foo?->method
        check_path_selection(
            "$.foo?->method",
            PathSelection {
                path: PathList::Var(
                    KnownVariable::Dollar.into_with_range(),
                    PathList::Key(
                        Key::field("foo").into_with_range(),
                        PathList::Question(
                            PathList::Method(
                                WithRange::new("method".to_string(), None),
                                None,
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
    }

    #[test]
    fn test_chained_optional_accesses() {
        // Test chained optional accesses: foo?.bar?.baz
        check_path_selection(
            "$.foo?.bar?.baz",
            PathSelection {
                path: PathList::Var(
                    KnownVariable::Dollar.into_with_range(),
                    PathList::Key(
                        Key::field("foo").into_with_range(),
                        PathList::Question(
                            PathList::Key(
                                Key::field("bar").into_with_range(),
                                PathList::Question(
                                    PathList::Key(
                                        Key::field("baz").into_with_range(),
                                        PathList::Empty.into_with_range(),
                                    )
                                    .into_with_range(),
                                )
                                .into_with_range(),
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
    }

    #[test]
    fn test_mixed_regular_and_optional_access() {
        // Test mixed regular and optional access: foo.bar?.baz
        check_path_selection(
            "$.foo.bar?.baz",
            PathSelection {
                path: PathList::Var(
                    KnownVariable::Dollar.into_with_range(),
                    PathList::Key(
                        Key::field("foo").into_with_range(),
                        PathList::Key(
                            Key::field("bar").into_with_range(),
                            PathList::Question(
                                PathList::Key(
                                    Key::field("baz").into_with_range(),
                                    PathList::Empty.into_with_range(),
                                )
                                .into_with_range(),
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
    }

    #[test]
    fn test_optional_chaining_with_subselection() {
        // Test optional chaining with subselection: foo?.bar { id name }
        check_path_selection(
            "$.foo?.bar { id name }",
            PathSelection {
                path: PathList::Var(
                    KnownVariable::Dollar.into_with_range(),
                    PathList::Key(
                        Key::field("foo").into_with_range(),
                        PathList::Question(
                            PathList::Key(
                                Key::field("bar").into_with_range(),
                                PathList::Selection(SubSelection {
                                    selections: vec![
                                        NamedSelection::Field(
                                            None,
                                            Key::field("id").into_with_range(),
                                            None,
                                        ),
                                        NamedSelection::Field(
                                            None,
                                            Key::field("name").into_with_range(),
                                            None,
                                        ),
                                    ],
                                    ..Default::default()
                                })
                                .into_with_range(),
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
    }

    #[test]
    fn test_optional_method_with_arguments() {
        // Test optional method with arguments: foo?->filter('active')
        check_path_selection(
            "$.foo?->filter('active')",
            PathSelection {
                path: PathList::Var(
                    KnownVariable::Dollar.into_with_range(),
                    PathList::Key(
                        Key::field("foo").into_with_range(),
                        PathList::Question(
                            PathList::Method(
                                WithRange::new("filter".to_string(), None),
                                Some(MethodArgs {
                                    args: vec![
                                        LitExpr::String("active".to_string()).into_with_range(),
                                    ],
                                    ..Default::default()
                                }),
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
    }
}
