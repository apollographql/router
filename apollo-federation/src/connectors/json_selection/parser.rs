use std::fmt::Display;
use std::hash::Hash;
use std::str::FromStr;

use apollo_compiler::collections::IndexSet;
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
use super::helpers::vec_push;
use super::known_var::KnownVariable;
use super::lit_expr::LitExpr;
use super::location::OffsetRange;
use super::location::Ranged;
use super::location::Span;
use super::location::SpanExtra;
use super::location::WithRange;
use super::location::merge_ranges;
use super::location::new_span_with_spec;
use super::location::ranged_span;
use crate::connectors::ConnectSpec;
use crate::connectors::Namespace;
use crate::connectors::json_selection::location::get_connect_spec;
use crate::connectors::json_selection::methods::ArrowMethod;
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
pub(super) fn nom_error_message(
    suffix: Span,
    // This message type forbids computing error messages with format!, which
    // might be worthwhile in the future. For now, it's convenient to avoid
    // String messages so the Span type can remain Copy, so we don't have to
    // clone spans frequently in the parsing code. In most cases, the suffix
    // provides the dynamic context needed to interpret the static message.
    message: impl Into<String>,
) -> nom::Err<nom::error::Error<Span>> {
    let offset = suffix.location_offset();
    nom::Err::Error(nom::error::Error::from_error_kind(
        suffix.map_extra(|extra| SpanExtra {
            errors: vec_push(extra.errors, (message.into(), offset)),
            ..extra
        }),
        nom::error::ErrorKind::IsNot,
    ))
}

// Generates a fatal error with the given suffix Span and message, causing the
// parser to abort with the given error message, which is useful after
// recognizing syntax that completely constrains what follows (like the -> token
// before a method name), and what follows does not parse as required.
pub(super) fn nom_fail_message(
    suffix: Span,
    message: impl Into<String>,
) -> nom::Err<nom::error::Error<Span>> {
    let offset = suffix.location_offset();
    nom::Err::Failure(nom::error::Error::from_error_kind(
        suffix.map_extra(|extra| SpanExtra {
            errors: vec_push(extra.errors, (message.into(), offset)),
            ..extra
        }),
        nom::error::ErrorKind::IsNot,
    ))
}

pub(crate) trait VarPaths {
    /// Implementers of `VarPaths` must implement this `var_paths` method, which
    /// should return all variable-referencing paths where the variable is a
    /// `KnownVariable::External(String)` or `KnownVariable::Local(String)`
    /// (that is, not internal variable references like `$` or `@`).
    fn var_paths(&self) -> Vec<&PathSelection>;

    fn external_var_paths(&self) -> Vec<&PathSelection> {
        self.var_paths()
            .into_iter()
            .filter(|var_path| {
                if let PathList::Var(known_var, _) = var_path.path.as_ref() {
                    matches!(known_var.as_ref(), KnownVariable::External(_))
                } else {
                    false
                }
            })
            .collect()
    }

    /// Returns all locally bound variable names in the selection, without
    /// regard for which ones are available where.
    fn local_var_names(&self) -> IndexSet<String> {
        self.var_paths()
            .into_iter()
            .flat_map(|var_path| {
                if let PathList::Var(known_var, _) = var_path.path.as_ref() {
                    match known_var.as_ref() {
                        KnownVariable::Local(var_name) => Some(var_name.to_string()),
                        _ => None,
                    }
                } else {
                    None
                }
            })
            .collect()
    }
}

// JSONSelection     ::= PathSelection | NakedSubSelection
// NakedSubSelection ::= NamedSelection* StarSelection?

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct JSONSelection {
    pub(super) inner: TopLevelSelection,
    pub spec: ConnectSpec,
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub(super) enum TopLevelSelection {
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

    // The ConnectSpec version used to parse and apply the selection.
    pub spec: ConnectSpec,
}

impl JSONSelection {
    pub fn spec(&self) -> ConnectSpec {
        self.spec
    }

    pub fn named(sub: SubSelection) -> Self {
        Self {
            inner: TopLevelSelection::Named(sub),
            spec: Self::default_connect_spec(),
        }
    }

    pub fn path(path: PathSelection) -> Self {
        Self {
            inner: TopLevelSelection::Path(path),
            spec: Self::default_connect_spec(),
        }
    }

    pub(crate) fn if_named_else_path<T>(
        &self,
        if_named: impl Fn(&SubSelection) -> T,
        if_path: impl Fn(&PathSelection) -> T,
    ) -> T {
        match &self.inner {
            TopLevelSelection::Named(subselect) => if_named(subselect),
            TopLevelSelection::Path(path) => if_path(path),
        }
    }

    pub fn empty() -> Self {
        Self {
            inner: TopLevelSelection::Named(SubSelection::default()),
            spec: Self::default_connect_spec(),
        }
    }

    pub fn is_empty(&self) -> bool {
        match &self.inner {
            TopLevelSelection::Named(subselect) => subselect.selections.is_empty(),
            TopLevelSelection::Path(path) => *path.path == PathList::Empty,
        }
    }

    // JSONSelection::parse is possibly the "most public" method in the entire
    // file, so it's important that the method signature can remain stable even
    // if we drastically change implementation details. That's why we use &str
    // as the input type and a custom JSONSelectionParseError type as the error
    // type, rather than using Span or nom::error::Error directly.
    pub fn parse(input: &str) -> Result<Self, JSONSelectionParseError> {
        JSONSelection::parse_with_spec(input, Self::default_connect_spec())
    }

    pub(super) fn default_connect_spec() -> ConnectSpec {
        ConnectSpec::V0_2
    }

    pub fn parse_with_spec(
        input: &str,
        spec: ConnectSpec,
    ) -> Result<Self, JSONSelectionParseError> {
        let span = new_span_with_spec(input, spec);

        match JSONSelection::parse_span(span) {
            Ok((remainder, selection)) => {
                let fragment = remainder.fragment();
                let produced_errors = !remainder.extra.errors.is_empty();
                if fragment.is_empty() && !produced_errors {
                    Ok(selection)
                } else {
                    let mut message = remainder
                        .extra
                        .errors
                        .iter()
                        .map(|(msg, _offset)| msg.as_str())
                        .collect::<Vec<_>>()
                        .join("\n");

                    // Use offset and fragment from first error if available
                    let (error_offset, error_fragment) =
                        if let Some((_, first_error_offset)) = remainder.extra.errors.first() {
                            let error_span =
                                new_span_with_spec(input, spec).slice(*first_error_offset..);
                            (
                                error_span.location_offset(),
                                error_span.fragment().to_string(),
                            )
                        } else {
                            (remainder.location_offset(), fragment.to_string())
                        };

                    if !fragment.is_empty() {
                        message
                            .push_str(&format!("\nUnexpected trailing characters: {}", fragment));
                    }
                    Err(JSONSelectionParseError {
                        message,
                        fragment: error_fragment,
                        offset: error_offset,
                        spec: remainder.extra.spec,
                    })
                }
            }

            Err(e) => match e {
                nom::Err::Error(e) | nom::Err::Failure(e) => Err(JSONSelectionParseError {
                    message: if e.input.extra.errors.is_empty() {
                        format!("nom::error::ErrorKind::{:?}", e.code)
                    } else {
                        e.input
                            .extra
                            .errors
                            .iter()
                            .map(|(msg, _offset)| msg.clone())
                            .join("\n")
                    },
                    fragment: e.input.fragment().to_string(),
                    offset: e.input.location_offset(),
                    spec: e.input.extra.spec,
                }),

                nom::Err::Incomplete(_) => unreachable!("nom::Err::Incomplete not expected here"),
            },
        }
    }

    fn parse_span(input: Span) -> ParseResult<Self> {
        match get_connect_spec(&input) {
            ConnectSpec::V0_1 | ConnectSpec::V0_2 => Self::parse_span_v0_2(input),
            ConnectSpec::V0_3 => Self::parse_span_v0_3(input),
        }
    }

    fn parse_span_v0_2(input: Span) -> ParseResult<Self> {
        let spec = get_connect_spec(&input);

        match alt((
            all_consuming(terminated(
                map(PathSelection::parse, |path| Self {
                    inner: TopLevelSelection::Path(path),
                    spec,
                }),
                // By convention, most ::parse methods do not consume trailing
                // spaces_or_comments, so we need to consume them here in order
                // to satisfy the all_consuming requirement.
                spaces_or_comments,
            )),
            all_consuming(terminated(
                map(SubSelection::parse_naked, |sub| Self {
                    inner: TopLevelSelection::Named(sub),
                    spec,
                }),
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

    fn parse_span_v0_3(input: Span) -> ParseResult<Self> {
        let spec = get_connect_spec(&input);

        match all_consuming(terminated(
            map(SubSelection::parse_naked, |sub| {
                if let (1, Some(only)) = (sub.selections.len(), sub.selections.first()) {
                    // SubSelection::parse_naked already enforces that there
                    // cannot be more than one NamedSelection if that
                    // NamedSelection is anonymous, and here's where we divert
                    // that case into TopLevelSelection::Path rather than
                    // TopLevelSelection::Named for easier processing later.
                    //
                    // The SubSelection may contain multiple inlined selections
                    // with NamingPrefix::Spread(None) (that is, an anonymous
                    // path with a trailing SubSelection), which are not
                    // considered anonymous in that context (because they may
                    // have zero or more output properties, which they spread
                    // into the larger result). However, if there is only one
                    // such ::Spread(None) selection in sub, then "spreading"
                    // its value into the larger SubSelection is equivalent to
                    // using its value as the entire output, so we can treat the
                    // whole thing as a TopLevelSelection::Path selection.
                    //
                    // Putting ... first causes NamingPrefix::Spread(Some(_)) to
                    // be used instead, so the whole selection remains a
                    // TopLevelSelection::Named, with the additional restriction
                    // that the argument of the ... must be an object or null
                    // (not an array). Eventually, we should deprecate spread
                    // selections without ..., and this complexity will go away.
                    if only.is_anonymous() || matches!(only.prefix, NamingPrefix::Spread(None)) {
                        return Self {
                            inner: TopLevelSelection::Path(only.path.clone()),
                            spec,
                        };
                    }
                }
                Self {
                    inner: TopLevelSelection::Named(sub),
                    spec,
                }
            }),
            // Most ::parse methods do not consume trailing spaces_or_comments,
            // but here (at the top level) we need to make sure anything left at
            // the end of the string is inconsequential, in order to satisfy the
            // all_consuming combinator above.
            spaces_or_comments,
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
        match &self.inner {
            TopLevelSelection::Named(subselect) => Some(subselect),
            TopLevelSelection::Path(path) => path.next_subselection(),
        }
    }

    #[allow(unused)]
    pub(crate) fn next_mut_subselection(&mut self) -> Option<&mut SubSelection> {
        match &mut self.inner {
            TopLevelSelection::Named(subselect) => Some(subselect),
            TopLevelSelection::Path(path) => path.next_mut_subselection(),
        }
    }

    pub fn variable_references(&self) -> impl Iterator<Item = VariableReference<Namespace>> + '_ {
        self.external_var_paths()
            .into_iter()
            .flat_map(|var_path| var_path.variable_reference())
    }
}

impl VarPaths for JSONSelection {
    fn var_paths(&self) -> Vec<&PathSelection> {
        match &self.inner {
            TopLevelSelection::Named(subselect) => subselect.var_paths(),
            TopLevelSelection::Path(path) => path.var_paths(),
        }
    }
}

// NamedSelection       ::= (Alias | "...")? PathSelection | Alias SubSelection
// PathSelection        ::= Path SubSelection?

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct NamedSelection {
    pub(super) prefix: NamingPrefix,
    pub(super) path: PathSelection,
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub(super) enum NamingPrefix {
    // When a NamedSelection has an Alias, it fully determines the output key,
    // and any applied values from the path will be assigned to that key.
    Alias(Alias),
    // A path can be spread without an explicit ... token, provided it has a
    // trailing SubSelection (guaranteeing it outputs a static set of object
    // properties). In those cases, the OffsetRange will be None. When there is
    // an actual ... token, the OffsetRange will be Some(token_range).
    Spread(OffsetRange),
    // When there is no Alias or ... spread token, and the path is not inlined
    // implicitly due to a trailing SubSelection (which would be represented by
    // ::Spread(None)), the NamingPrefix is ::None. The NamedSelection may still
    // produce a single output key if self.path.get_single_key() returns
    // Some(key), but otherwise it's an anonymous path, which produces only a
    // JSON value. Singular anonymous paths are allowed at the top level, where
    // any value they produce directly determines the output of the selection,
    // but anonymous NamedSelections cannot be mixed together with other
    // NamedSelections that produce names (in a SubSelection or anywhere else).
    None,
}

// Like PathSelection, NamedSelection is an AST structure that takes its range
// entirely from its children, so NamedSelection itself does not need to provide
// separate storage for its own range, and therefore does not need to be wrapped
// as WithRange<NamedSelection>, but merely needs to implement the Ranged trait.
impl Ranged for NamedSelection {
    fn range(&self) -> OffsetRange {
        let alias_or_spread_range = match &self.prefix {
            NamingPrefix::None => None,
            NamingPrefix::Alias(alias) => alias.range(),
            NamingPrefix::Spread(range) => range.clone(),
        };
        merge_ranges(alias_or_spread_range, self.path.range())
    }
}

impl NamedSelection {
    pub(super) fn has_single_output_key(&self) -> bool {
        self.get_single_key().is_some()
    }

    pub(super) fn get_single_key(&self) -> Option<&WithRange<Key>> {
        match &self.prefix {
            NamingPrefix::None => self.path.get_single_key(),
            NamingPrefix::Spread(_) => None,
            NamingPrefix::Alias(alias) => Some(&alias.name),
        }
    }

    pub(super) fn is_anonymous(&self) -> bool {
        match &self.prefix {
            NamingPrefix::None => self.path.is_anonymous(),
            NamingPrefix::Alias(_) => false,
            NamingPrefix::Spread(_) => false,
        }
    }

    pub(super) fn field(
        alias: Option<Alias>,
        name: WithRange<Key>,
        selection: Option<SubSelection>,
    ) -> Self {
        let name_range = name.range();
        let tail = if let Some(selection) = selection.as_ref() {
            WithRange::new(PathList::Selection(selection.clone()), selection.range())
        } else {
            // The empty range is a collapsed range at the end of the
            // preceding path, i.e. at the end of the field name.
            let empty_range = name_range.as_ref().map(|range| range.end..range.end);
            WithRange::new(PathList::Empty, empty_range)
        };
        let tail_range = tail.range();
        let name_tail_range = merge_ranges(name_range, tail_range);
        let prefix = if let Some(alias) = alias {
            NamingPrefix::Alias(alias)
        } else {
            NamingPrefix::None
        };
        Self {
            prefix,
            path: PathSelection {
                path: WithRange::new(PathList::Key(name, tail), name_tail_range),
            },
        }
    }

    pub(crate) fn parse(input: Span) -> ParseResult<Self> {
        match get_connect_spec(&input) {
            ConnectSpec::V0_1 | ConnectSpec::V0_2 => Self::parse_v0_2(input),
            ConnectSpec::V0_3 => Self::parse_v0_3(input),
        }
    }

    pub(crate) fn parse_v0_2(input: Span) -> ParseResult<Self> {
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
            (remainder, Self::field(alias, name, selection))
        })
    }

    // Parses either NamedPathSelection or PathWithSubSelection.
    fn parse_path(input: Span) -> ParseResult<Self> {
        if let Ok((remainder, alias)) = Alias::parse(input.clone()) {
            match PathSelection::parse(remainder) {
                Ok((remainder, path)) => Ok((
                    remainder,
                    Self {
                        prefix: NamingPrefix::Alias(alias),
                        path,
                    },
                )),
                Err(nom::Err::Failure(e)) => Err(nom::Err::Failure(e)),
                Err(_) => Err(nom_error_message(
                    input.clone(),
                    "Path selection alias must be followed by a path",
                )),
            }
        } else {
            match PathSelection::parse(input.clone()) {
                Ok((remainder, path)) => {
                    if path.is_anonymous() && path.has_subselection() {
                        // This covers the old PathWithSubSelection syntax,
                        // which is like ... in behavior (object properties
                        // spread into larger object) but without the explicit
                        // ... token. This syntax still works, provided the path
                        // is both anonymous and has a trailing SubSelection.
                        Ok((
                            remainder,
                            Self {
                                prefix: NamingPrefix::Spread(None),
                                path,
                            },
                        ))
                    } else {
                        Err(nom_fail_message(
                            input.clone(),
                            "Named path selection must either begin with alias or ..., or end with subselection",
                        ))
                    }
                }
                Err(nom::Err::Failure(e)) => Err(nom::Err::Failure(e)),
                Err(_) => Err(nom_error_message(
                    input.clone(),
                    "Path selection must either begin with alias or ..., or end with subselection",
                )),
            }
        }
    }

    fn parse_group(input: Span) -> ParseResult<Self> {
        tuple((Alias::parse, SubSelection::parse))(input).map(|(input, (alias, group))| {
            let group_range = group.range();
            (
                input,
                NamedSelection {
                    prefix: NamingPrefix::Alias(alias),
                    path: PathSelection {
                        path: WithRange::new(PathList::Selection(group), group_range),
                    },
                },
            )
        })
    }

    // TODO Reenable ... in ConnectSpec::V0_4, to support abstract types.
    // NamedSelection ::= (Alias | "...")? PathSelection | Alias SubSelection
    fn parse_v0_3(input: Span) -> ParseResult<Self> {
        let spec = get_connect_spec(&input);
        let (after_alias, alias) = opt(Alias::parse)(input.clone())?;

        if let Some(alias) = alias {
            if let Ok((remainder, sub)) = SubSelection::parse(after_alias.clone()) {
                let sub_range = sub.range();
                return Ok((
                    remainder,
                    Self {
                        prefix: NamingPrefix::Alias(alias),
                        // This is what used to be called a NamedGroupSelection
                        // in the grammar, where an Alias SubSelection can be
                        // used to assign a nested name (the Alias) to a
                        // selection of fields from the current object.
                        // Logically, this corresponds to an Alias followed by a
                        // PathSelection with an empty/missing Path. While there
                        // is no way to write such a PathSelection normally, we
                        // can construct a PathList consisting of only a
                        // SubSelection here, for the sake of using the same
                        // machinery to process all NamedSelection nodes.
                        path: PathSelection {
                            path: WithRange::new(PathList::Selection(sub), sub_range),
                        },
                    },
                ));
            }

            PathSelection::parse(after_alias.clone()).map(|(remainder, path)| {
                (
                    remainder,
                    Self {
                        prefix: NamingPrefix::Alias(alias),
                        path,
                    },
                )
            })
        } else {
            tuple((
                spaces_or_comments,
                opt(ranged_span("...")),
                PathSelection::parse,
            ))(input.clone())
            .map(|(mut remainder, (_spaces, spread, path))| {
                let prefix = if let Some(spread) = spread {
                    if spec <= ConnectSpec::V0_3 {
                        remainder.extra.errors.push((
                            "Spread syntax (...) is planned for connect/v0.4".to_string(),
                            input.location_offset(),
                        ));
                    }
                    // An explicit ... spread token was used, so we record
                    // NamingPrefix::Spread(Some(_)). If the path produces
                    // something other than an object or null, we will catch
                    // that in apply_to_path and compute_output_shape (not a
                    // parsing concern).
                    NamingPrefix::Spread(spread.range())
                } else if path.is_anonymous() && path.has_subselection() {
                    // If there is no Alias or ... and the path is anonymous and
                    // it has a trailing SubSelection, then it should be spread
                    // into the larger SubSelection. This is an older syntax
                    // (PathWithSubSelection) that provided some of the benefits
                    // of ..., before ... was supported (in connect/v0.3). It's
                    // important the path is anonymous, since regular field
                    // selections like `user { id name }` meet all the criteria
                    // above but should not be spread because they do produce an
                    // output key.
                    NamingPrefix::Spread(None)
                } else {
                    // Otherwise, the path has no prefix, so it either produces
                    // a single Key according to path.get_single_key(), or this
                    // is an anonymous NamedSelection, which are only allowed at
                    // the top level. However, since we don't know about other
                    // NamedSelections here, these rules have to be enforced at
                    // a higher level.
                    NamingPrefix::None
                };
                (remainder, Self { prefix, path })
            })
        }
    }

    pub(crate) fn names(&self) -> Vec<&str> {
        if let Some(single_key) = self.get_single_key() {
            vec![single_key.as_str()]
        } else if let Some(sub) = self.path.next_subselection() {
            // Flatten and deduplicate the names of the NamedSelection
            // items in the SubSelection.
            let mut name_set = IndexSet::default();
            for selection in sub.selections_iter() {
                name_set.extend(selection.names());
            }
            name_set.into_iter().collect()
        } else {
            Vec::new()
        }
    }

    /// Find the next subselection, if present
    pub(crate) fn next_subselection(&self) -> Option<&SubSelection> {
        self.path.next_subselection()
    }

    #[allow(unused)]
    pub(crate) fn next_mut_subselection(&mut self) -> Option<&mut SubSelection> {
        self.path.next_mut_subselection()
    }
}

impl VarPaths for NamedSelection {
    fn var_paths(&self) -> Vec<&PathSelection> {
        self.path.var_paths()
    }
}

// Path                 ::= VarPath | KeyPath | AtPath | ExprPath
// PathSelection        ::= Path SubSelection?
// VarPath              ::= "$" (NO_SPACE Identifier)? PathTail
// KeyPath              ::= Key PathTail
// AtPath               ::= "@" PathTail
// ExprPath             ::= "$(" LitExpr ")" PathTail
// PathTail             ::= "?"? (PathStep "?"?)*
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
    pub(crate) fn parse(input: Span) -> ParseResult<Self> {
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

    pub(super) fn get_single_key(&self) -> Option<&WithRange<Key>> {
        self.path.get_single_key()
    }

    pub(super) fn is_anonymous(&self) -> bool {
        self.path.is_anonymous()
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

impl VarPaths for PathSelection {
    fn var_paths(&self) -> Vec<&PathSelection> {
        let mut paths = Vec::new();
        match self.path.as_ref() {
            PathList::Var(var_name, tail) => {
                // At this point, we're collecting both external and local
                // variable references (but not references to internal variables
                // like $ and @). These mixed variables will be filtered in
                // VarPaths::external_var_paths and ::local_var_paths.
                if matches!(
                    var_name.as_ref(),
                    KnownVariable::External(_) | KnownVariable::Local(_)
                ) {
                    paths.push(self);
                }
                paths.extend(tail.var_paths());
            }
            other => {
                paths.extend(other.var_paths());
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
pub(crate) enum PathList {
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

    // Represents the ? syntax used for some.path?->method(...) optional
    // chaining. If the preceding some.path value is missing (None) or null,
    // some.path? evaluates to None, terminating path evaluation without an
    // error. All other (non-null) values are passed along without change.
    //
    // The WithRange<PathList> parameter represents the rest of the path
    // following the `?` token.
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
        match Self::parse_with_depth(input.clone(), 0) {
            Ok((_, parsed)) if matches!(*parsed, Self::Empty) => Err(nom_error_message(
                input.clone(),
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
        let spec = get_connect_spec(&input);

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
            match tuple((
                spaces_or_comments,
                ranged_span("$("),
                LitExpr::parse,
                spaces_or_comments,
                ranged_span(")"),
            ))(input.clone())
            {
                Ok((suffix, (_, dollar_open_paren, expr, close_paren, _))) => {
                    let (remainder, rest) = Self::parse_with_depth(suffix, depth + 1)?;
                    let expr_range = merge_ranges(dollar_open_paren.range(), close_paren.range());
                    let full_range = merge_ranges(expr_range, rest.range());
                    return Ok((
                        remainder,
                        WithRange::new(Self::Expr(expr, rest), full_range),
                    ));
                }
                Err(nom::Err::Failure(err)) => {
                    return Err(nom::Err::Failure(err));
                }
                Err(_) => {
                    // We can otherwise continue for non-fatal errors
                }
            }

            if let Ok((suffix, (dollar, opt_var))) =
                tuple((ranged_span("$"), opt(parse_identifier_no_space)))(input.clone())
            {
                let dollar_range = dollar.range();
                let (remainder, rest) = Self::parse_with_depth(suffix, depth + 1)?;
                let full_range = merge_ranges(dollar_range.clone(), rest.range());
                return if let Some(var) = opt_var {
                    let full_name = format!("{}{}", dollar.as_ref(), var.as_str());
                    // This KnownVariable::External variant may get remapped to
                    // KnownVariable::Local if the variable was parsed as the
                    // first argument of an input->as($var) method call.
                    let known_var = if input.extra.is_local_var(&full_name) {
                        KnownVariable::Local(full_name)
                    } else {
                        KnownVariable::External(full_name)
                    };
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

            if let Ok((suffix, at)) = ranged_span("@")(input.clone()) {
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

            if let Ok((suffix, key)) = Key::parse(input.clone()) {
                let (remainder, rest) = Self::parse_with_depth(suffix, depth + 1)?;

                return match spec {
                    ConnectSpec::V0_1 | ConnectSpec::V0_2 => match rest.as_ref() {
                        // We use nom_error_message rather than nom_fail_message
                        // here because the key might actually be a field selection,
                        // which means we want to unwind parsing the path and fall
                        // back to parsing other kinds of NamedSelection.
                        Self::Empty | Self::Selection(_) => Err(nom_error_message(
                            input.clone(),
                            // Another place where format! might be useful to
                            // suggest .{key}, which would require storing error
                            // messages as owned Strings.
                            "Single-key path must be prefixed with $. to avoid ambiguity with field name",
                        )),
                        _ => {
                            let full_range = merge_ranges(key.range(), rest.range());
                            Ok((remainder, WithRange::new(Self::Key(key, rest), full_range)))
                        }
                    },

                    // With the unification of NamedSelection enum variants into
                    // a single struct in connect/v0.3, the ambiguity between
                    // single-key paths and field selections is no longer a
                    // problem, since they are now represented the same way.
                    ConnectSpec::V0_3 => {
                        let full_range = merge_ranges(key.range(), rest.range());
                        Ok((remainder, WithRange::new(Self::Key(key, rest), full_range)))
                    }
                };
            }
        }

        if depth == 0 {
            // If the PathSelection does not start with a $var (or $ or @), a
            // key., or $(expr), it is not a valid PathSelection.
            if tuple((ranged_span("."), Key::parse))(input.clone()).is_ok() {
                // Since we previously allowed starting key paths with .key but
                // now forbid that syntax (because it can be ambiguous), suggest
                // the unambiguous $.key syntax instead.
                return Err(nom_fail_message(
                    input.clone(),
                    "Key paths cannot start with just .key (use $.key instead)",
                ));
            }
            // This error technically covers the case above, but doesn't suggest
            // a helpful solution.
            return Err(nom_error_message(
                input.clone(),
                "Path selection must start with key, $variable, $, @, or $(expression)",
            ));
        }

        // At any depth, if the next token is ? but not the PathList::Question
        // kind, we terminate path parsing so the hypothetical ?? or ?! tokens
        // have a chance to be parsed as infix operators. This is not
        // version-gated to connect/v0.3, because we want to begin forbidding
        // these tokens as continuations of a Path as early as we can.
        if input.fragment().starts_with("??") || input.fragment().starts_with("?!") {
            return Ok((input, WithRange::new(Self::Empty, range_if_empty)));
        }

        match spec {
            ConnectSpec::V0_1 | ConnectSpec::V0_2 => {
                // The ? token was not introduced until connect/v0.3.
            }
            ConnectSpec::V0_3 => {
                if let Ok((suffix, question)) = ranged_span("?")(input.clone()) {
                    let (remainder, rest) = Self::parse_with_depth(suffix.clone(), depth + 1)?;

                    return match rest.as_ref() {
                        // The ? cannot be repeated sequentially, so if rest starts with
                        // another PathList::Question, we terminate the current path,
                        // probably (but not necessarily) leading to a parse error for
                        // the upcoming ?.
                        PathList::Question(_) => {
                            let empty_range = question.range().map(|range| range.end..range.end);
                            let empty = WithRange::new(Self::Empty, empty_range);
                            Ok((
                                suffix,
                                WithRange::new(Self::Question(empty), question.range()),
                            ))
                        }
                        _ => {
                            let full_range = merge_ranges(question.range(), rest.range());
                            Ok((remainder, WithRange::new(Self::Question(rest), full_range)))
                        }
                    };
                }
            }
        };

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
        if let Ok((remainder, (dot, key))) = tuple((ranged_span("."), Key::parse))(input.clone()) {
            let (remainder, rest) = Self::parse_with_depth(remainder, depth + 1)?;
            let dot_key_range = merge_ranges(dot.range(), key.range());
            let full_range = merge_ranges(dot_key_range, rest.range());
            return Ok((remainder, WithRange::new(Self::Key(key, rest), full_range)));
        }

        // If we failed to parse "." Key above, but the input starts with a '.'
        // character, it's an error unless it's the beginning of a ... token.
        if input.fragment().starts_with('.') && !input.fragment().starts_with("...") {
            return Err(nom_fail_message(
                input.clone(),
                "Path selection . must be followed by key (identifier or quoted string literal)",
            ));
        }

        // PathSelection can never start with a naked ->method (instead, use
        // $->method or @->method if you want to operate on the current value).
        if let Ok((suffix, arrow)) = ranged_span("->")(input.clone()) {
            // As soon as we see a -> token, we know what follows must be a
            // method name, so we can unconditionally return based on what
            // parse_identifier tells us. since MethodArgs::parse is optional,
            // the absence of args will never trigger the error case.
            return match tuple((parse_identifier, opt(MethodArgs::parse)))(suffix) {
                Ok((suffix, (method, args_opt))) => {
                    let mut local_var_name = None;

                    // Convert the first argument of input->as($var) from
                    // KnownVariable::External (the default for parsed named
                    // variable references) to KnownVariable::Local, when we know
                    // we're parsing an ->as($var) method invocation.
                    let args = if let Some(args) = args_opt.as_ref()
                        && ArrowMethod::lookup(method.as_ref()) == Some(ArrowMethod::As)
                    {
                        let new_args = if let Some(old_first_arg) = args.args.first()
                            && let LitExpr::Path(path_selection) = old_first_arg.as_ref()
                            && let PathList::Var(var_name, var_tail) = path_selection.path.as_ref()
                            && let KnownVariable::External(var_str) | KnownVariable::Local(var_str) =
                                var_name.as_ref()
                        {
                            let as_var = WithRange::new(
                                // This is the key change: remap to KnownVariable::Local.
                                KnownVariable::Local(var_str.clone()),
                                var_name.range(),
                            );

                            local_var_name = Some(var_str.clone());

                            let new_first_arg = WithRange::new(
                                LitExpr::Path(PathSelection {
                                    path: WithRange::new(
                                        PathList::Var(as_var, var_tail.clone()),
                                        path_selection.range(),
                                    ),
                                }),
                                old_first_arg.range(),
                            );

                            let mut new_args = vec![new_first_arg];
                            new_args.extend(args.args.iter().skip(1).cloned());
                            new_args
                        } else {
                            args.args.clone()
                        };

                        Some(MethodArgs {
                            args: new_args,
                            range: args.range(),
                        })
                    } else {
                        args_opt
                    };

                    let suffix_with_local_var = if let Some(var_name) = local_var_name {
                        suffix.map_extra(|extra| extra.with_local_var(var_name))
                    } else {
                        suffix
                    };

                    let (remainder, rest) =
                        Self::parse_with_depth(suffix_with_local_var, depth + 1)?;
                    let full_range = merge_ranges(arrow.range(), rest.range());

                    Ok((
                        remainder,
                        WithRange::new(Self::Method(method, args, rest), full_range),
                    ))
                }
                Err(_) => Err(nom_fail_message(
                    input.clone(),
                    "Method name must follow ->",
                )),
            };
        }

        // Likewise, if the PathSelection has a SubSelection, it must appear at
        // the end of a non-empty path. PathList::parse_with_depth is not
        // responsible for enforcing a trailing SubSelection in the
        // PathWithSubSelection case, since that requirement is checked by
        // NamedSelection::parse_path.
        if let Ok((suffix, selection)) = SubSelection::parse(input.clone()) {
            let selection_range = selection.range();
            return Ok((
                suffix,
                WithRange::new(Self::Selection(selection), selection_range),
            ));
        }

        // The Self::Empty enum case is used to indicate the end of a
        // PathSelection that has no SubSelection.
        Ok((input.clone(), WithRange::new(Self::Empty, range_if_empty)))
    }

    pub(super) fn is_anonymous(&self) -> bool {
        self.get_single_key().is_none()
    }

    pub(super) fn is_single_key(&self) -> bool {
        self.get_single_key().is_some()
    }

    pub(super) fn get_single_key(&self) -> Option<&WithRange<Key>> {
        fn rest_is_empty_or_selection(rest: &WithRange<PathList>) -> bool {
            match rest.as_ref() {
                PathList::Selection(_) | PathList::Empty => true,
                PathList::Question(tail) => rest_is_empty_or_selection(tail),
                // We could have a `_ => false` catch-all case here, but relying
                // on the exhaustiveness of this match ensures additions of new
                // PathList variants in the future (e.g. PathList::Question)
                // will be nudged to consider whether they should be compatible
                // with single-key field selections.
                PathList::Var(_, _)
                | PathList::Key(_, _)
                | PathList::Expr(_, _)
                | PathList::Method(_, _, _) => false,
            }
        }

        match self {
            Self::Key(key, key_rest) => {
                if rest_is_empty_or_selection(key_rest) {
                    Some(key)
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    pub(super) fn is_question(&self) -> bool {
        matches!(self, Self::Question(_))
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

impl VarPaths for PathList {
    fn var_paths(&self) -> Vec<&PathSelection> {
        let mut paths = Vec::new();
        match self {
            // PathSelection::var_paths is responsible for adding all
            // variable &PathSelection items to the set, since this
            // PathList::Var case cannot be sure it's looking at the beginning
            // of the path. However, we call rest.var_paths()
            // recursively because the tail of the list could contain other full
            // PathSelection variable references.
            PathList::Var(_, rest) | PathList::Key(_, rest) => {
                paths.extend(rest.var_paths());
            }
            PathList::Expr(expr, rest) => {
                paths.extend(expr.var_paths());
                paths.extend(rest.var_paths());
            }
            PathList::Method(_, opt_args, rest) => {
                if let Some(args) = opt_args {
                    for lit_arg in &args.args {
                        paths.extend(lit_arg.var_paths());
                    }
                }
                paths.extend(rest.var_paths());
            }
            PathList::Question(rest) => {
                paths.extend(rest.var_paths());
            }
            PathList::Selection(sub) => paths.extend(sub.var_paths()),
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
        match many0(NamedSelection::parse)(input.clone()) {
            Ok((remainder, selections)) => {
                // Enforce that if selections has any anonymous NamedSelection
                // elements, there is only one and it's the only NamedSelection in
                // the SubSelection.
                for sel in selections.iter() {
                    if sel.is_anonymous() && selections.len() > 1 {
                        return Err(nom_error_message(
                            input.clone(),
                            "SubSelection cannot contain multiple elements if it contains an anonymous NamedSelection",
                        ));
                    }
                }

                let range = merge_ranges(
                    selections.first().and_then(|first| first.range()),
                    selections.last().and_then(|last| last.range()),
                );

                Ok((remainder, Self { selections, range }))
            }
            Err(e) => Err(e),
        }
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
            if selection.has_single_output_key() {
                // If the PathSelection has an Alias, then it has a singular
                // name and should be visited directly.
                selections.push(selection);
            } else if let Some(sub) = selection.path.next_subselection() {
                // If the PathSelection does not have an Alias but does have a
                // SubSelection, then it represents the PathWithSubSelection
                // non-terminal from the grammar (see README.md + PR #6076),
                // which produces multiple names derived from the SubSelection,
                // which need to be recursively collected.
                selections.extend(sub.selections_iter());
            } else {
                // This no-Alias, no-SubSelection case should be forbidden by
                // NamedSelection::parse_path.
                debug_assert!(false, "PathSelection without Alias or SubSelection");
            }
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

impl VarPaths for SubSelection {
    fn var_paths(&self) -> Vec<&PathSelection> {
        let mut paths = Vec::new();
        for selection in &self.selections {
            paths.extend(selection.var_paths());
        }
        paths
    }
}

// Alias ::= Key ":"

#[derive(Debug, PartialEq, Eq, Clone)]
pub(crate) struct Alias {
    pub(super) name: WithRange<Key>,
    pub(super) range: OffsetRange,
}

impl Ranged for Alias {
    fn range(&self) -> OffsetRange {
        self.range.clone()
    }
}

impl Alias {
    pub(crate) fn new(name: &str) -> Self {
        if is_identifier(name) {
            Self::field(name)
        } else {
            Self::quoted(name)
        }
    }

    pub(crate) fn field(name: &str) -> Self {
        Self {
            name: WithRange::new(Key::field(name), None),
            range: None,
        }
    }

    pub(crate) fn quoted(name: &str) -> Self {
        Self {
            name: WithRange::new(Key::quoted(name), None),
            range: None,
        }
    }

    pub(crate) fn parse(input: Span) -> ParseResult<Self> {
        tuple((Key::parse, spaces_or_comments, ranged_span(":")))(input).map(
            |(input, (name, _, colon))| {
                let range = merge_ranges(name.range(), colon.range());
                (input, Self { name, range })
            },
        )
    }
}

// Key ::= Identifier | LitString

#[derive(Debug, PartialEq, Eq, Clone, Hash)]
pub enum Key {
    Field(String),
    Quoted(String),
}

impl Key {
    pub(crate) fn parse(input: Span) -> ParseResult<WithRange<Self>> {
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
    // TODO Don't use the whole parser for this?
    all_consuming(parse_identifier_no_space)(new_span_with_spec(
        input,
        JSONSelection::default_connect_spec(),
    ))
    .is_ok()
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
                    let range = Some(start..remainder.location_offset());
                    (
                        remainder,
                        WithRange::new(chars.iter().collect::<String>(), range),
                    )
                })
        }

        _ => Err(nom_error_message(input, "Not a string literal")),
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Default)]
pub(crate) struct MethodArgs {
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
        if let Ok((remainder, first)) = LitExpr::parse(input.clone()) {
            args.push(first);
            input = remainder;

            while let Ok((remainder, _)) = tuple((spaces_or_comments, char(',')))(input.clone()) {
                input = spaces_or_comments(remainder)?.0;
                if let Ok((remainder, arg)) = LitExpr::parse(input.clone()) {
                    args.push(arg);
                    input = remainder;
                } else {
                    break;
                }
            }
        }

        input = spaces_or_comments(input.clone())?.0;
        let (input, close_paren) = ranged_span(")")(input.clone())?;

        let range = merge_ranges(open_paren.range(), close_paren.range());
        Ok((input, Self { args, range }))
    }
}

#[cfg(test)]
mod tests {
    use apollo_compiler::collections::IndexMap;
    use rstest::rstest;

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
    fn test_default_connect_spec() {
        // We don't necessarily want to update what
        // JSONSelection::default_connect_spec() returns just because
        // ConnectSpec::latest() changes, but we want to know when it happens,
        // so we can consider updating.
        assert_eq!(JSONSelection::default_connect_spec(), ConnectSpec::latest());
    }

    #[test]
    fn test_identifier() {
        fn check(input: &str, expected_name: &str) {
            let (remainder, name) = parse_identifier(new_span(input)).unwrap();
            assert!(
                span_is_all_spaces_or_comments(remainder.clone()),
                "remainder is `{:?}`",
                remainder.clone(),
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
                parse_identifier_no_space(identifier_with_leading_space.clone()),
                Err(nom::Err::Error(nom::error::Error::from_error_kind(
                    // The parse_identifier_no_space function does not provide a
                    // custom error message, since it's only used internally.
                    // Testing it directly here is somewhat contrived.
                    identifier_with_leading_space.clone(),
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
                span_is_all_spaces_or_comments(remainder.clone()),
                "remainder is `{:?}`",
                remainder.clone(),
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
                span_is_all_spaces_or_comments(remainder.clone()),
                "remainder is `{:?}`",
                remainder.clone(),
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
                span_is_all_spaces_or_comments(remainder.clone()),
                "remainder is `{:?}`",
                remainder.clone(),
            );
            assert_eq!(parsed.name.as_str(), alias);
        }

        check("hello:", "hello");
        check("hello :", "hello");
        check("hello : ", "hello");
        check("  hello :", "hello");
        check("hello: ", "hello");
    }

    #[test]
    fn test_named_selection() {
        #[track_caller]
        fn assert_result_and_names(input: &str, expected: NamedSelection, names: &[&str]) {
            let (remainder, selection) = NamedSelection::parse(new_span(input)).unwrap();
            assert!(
                span_is_all_spaces_or_comments(remainder.clone()),
                "remainder is `{:?}`",
                remainder.clone(),
            );
            let selection = selection.strip_ranges();
            assert_eq!(selection, expected);
            assert_eq!(selection.names(), names);
            assert_eq!(
                selection!(input).strip_ranges(),
                JSONSelection::named(SubSelection {
                    selections: vec![expected],
                    ..Default::default()
                },),
            );
        }

        assert_result_and_names(
            "hello",
            NamedSelection::field(None, Key::field("hello").into_with_range(), None),
            &["hello"],
        );

        assert_result_and_names(
            "hello { world }",
            NamedSelection::field(
                None,
                Key::field("hello").into_with_range(),
                Some(SubSelection {
                    selections: vec![NamedSelection::field(
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
            NamedSelection::field(
                Some(Alias::new("hi")),
                Key::field("hello").into_with_range(),
                None,
            ),
            &["hi"],
        );

        assert_result_and_names(
            "hi: 'hello world'",
            NamedSelection::field(
                Some(Alias::new("hi")),
                Key::quoted("hello world").into_with_range(),
                None,
            ),
            &["hi"],
        );

        assert_result_and_names(
            "hi: hello { world }",
            NamedSelection::field(
                Some(Alias::new("hi")),
                Key::field("hello").into_with_range(),
                Some(SubSelection {
                    selections: vec![NamedSelection::field(
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
            NamedSelection::field(
                Some(Alias::new("hey")),
                Key::field("hello").into_with_range(),
                Some(SubSelection {
                    selections: vec![
                        NamedSelection::field(None, Key::field("world").into_with_range(), None),
                        NamedSelection::field(None, Key::field("again").into_with_range(), None),
                    ],
                    ..Default::default()
                }),
            ),
            &["hey"],
        );

        assert_result_and_names(
            "hey: 'hello world' { again }",
            NamedSelection::field(
                Some(Alias::new("hey")),
                Key::quoted("hello world").into_with_range(),
                Some(SubSelection {
                    selections: vec![NamedSelection::field(
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
            NamedSelection::field(
                Some(Alias::new("leggo")),
                Key::quoted("my ego").into_with_range(),
                None,
            ),
            &["leggo"],
        );

        assert_result_and_names(
            "'let go': 'my ego'",
            NamedSelection::field(
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
            JSONSelection::named(SubSelection {
                selections: vec![],
                ..Default::default()
            }),
        );

        assert_eq!(
            selection!("   ").strip_ranges(),
            JSONSelection::named(SubSelection {
                selections: vec![],
                ..Default::default()
            }),
        );

        assert_eq!(
            selection!("hello").strip_ranges(),
            JSONSelection::named(SubSelection {
                selections: vec![NamedSelection::field(
                    None,
                    Key::field("hello").into_with_range(),
                    None
                )],
                ..Default::default()
            }),
        );

        assert_eq!(
            selection!("$.hello").strip_ranges(),
            JSONSelection::path(PathSelection {
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
            let expected = JSONSelection::named(SubSelection {
                selections: vec![NamedSelection {
                    prefix: NamingPrefix::Alias(Alias::new("hi")),
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
            let expected = JSONSelection::named(SubSelection {
                selections: vec![
                    NamedSelection::field(None, Key::field("before").into_with_range(), None),
                    NamedSelection {
                        prefix: NamingPrefix::Alias(Alias::new("hi")),
                        path: PathSelection::from_slice(
                            &[
                                Key::Field("hello".to_string()),
                                Key::Field("world".to_string()),
                            ],
                            None,
                        ),
                    },
                    NamedSelection::field(None, Key::field("after").into_with_range(), None),
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
            let expected = JSONSelection::named(SubSelection {
                selections: vec![
                    NamedSelection::field(None, Key::field("before").into_with_range(), None),
                    NamedSelection {
                        prefix: NamingPrefix::Alias(Alias::new("hi")),
                        path: PathSelection::from_slice(
                            &[
                                Key::Field("hello".to_string()),
                                Key::Field("world".to_string()),
                            ],
                            Some(SubSelection {
                                selections: vec![
                                    NamedSelection::field(
                                        None,
                                        Key::field("nested").into_with_range(),
                                        None,
                                    ),
                                    NamedSelection::field(
                                        None,
                                        Key::field("names").into_with_range(),
                                        None,
                                    ),
                                ],
                                ..Default::default()
                            }),
                        ),
                    },
                    NamedSelection::field(None, Key::field("after").into_with_range(), None),
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
    fn check_path_selection(spec: ConnectSpec, input: &str, expected: PathSelection) {
        let (remainder, path_selection) =
            PathSelection::parse(new_span_with_spec(input, spec)).unwrap();
        assert!(
            span_is_all_spaces_or_comments(remainder.clone()),
            "remainder is `{:?}`",
            remainder.clone(),
        );
        let path_without_ranges = path_selection.strip_ranges();
        assert_eq!(&path_without_ranges, &expected);
        assert_eq!(
            selection!(input, spec).strip_ranges(),
            JSONSelection {
                inner: TopLevelSelection::Path(path_without_ranges),
                spec,
            },
        );
    }

    #[rstest]
    #[case::v0_2(ConnectSpec::V0_2)]
    #[case::v0_3(ConnectSpec::V0_3)]
    fn test_path_selection(#[case] spec: ConnectSpec) {
        check_path_selection(
            spec,
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
            check_path_selection(spec, "$.hello.world", expected.clone());
            check_path_selection(spec, "$.hello .world", expected.clone());
            check_path_selection(spec, "$.hello. world", expected.clone());
            check_path_selection(spec, "$.hello . world", expected.clone());
            check_path_selection(spec, "$ . hello . world", expected.clone());
            check_path_selection(spec, " $ . hello . world ", expected);
        }

        {
            let expected = PathSelection::from_slice(
                &[
                    Key::Field("hello".to_string()),
                    Key::Field("world".to_string()),
                ],
                None,
            );
            check_path_selection(spec, "hello.world", expected.clone());
            check_path_selection(spec, "hello .world", expected.clone());
            check_path_selection(spec, "hello. world", expected.clone());
            check_path_selection(spec, "hello . world", expected.clone());
            check_path_selection(spec, " hello . world ", expected);
        }

        {
            let expected = PathSelection::from_slice(
                &[
                    Key::Field("hello".to_string()),
                    Key::Field("world".to_string()),
                ],
                Some(SubSelection {
                    selections: vec![NamedSelection::field(
                        None,
                        Key::field("hello").into_with_range(),
                        None,
                    )],
                    ..Default::default()
                }),
            );
            check_path_selection(spec, "hello.world{hello}", expected.clone());
            check_path_selection(spec, "hello.world { hello }", expected.clone());
            check_path_selection(spec, "hello .world { hello }", expected.clone());
            check_path_selection(spec, "hello. world { hello }", expected.clone());
            check_path_selection(spec, "hello . world { hello }", expected.clone());
            check_path_selection(spec, " hello . world { hello } ", expected);
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
                spec,
                "nested.'string literal'.\"property\".name",
                expected.clone(),
            );
            check_path_selection(
                spec,
                "nested. 'string literal'.\"property\".name",
                expected.clone(),
            );
            check_path_selection(
                spec,
                "nested.'string literal'. \"property\".name",
                expected.clone(),
            );
            check_path_selection(
                spec,
                "nested.'string literal'.\"property\" .name",
                expected.clone(),
            );
            check_path_selection(
                spec,
                "nested.'string literal'.\"property\". name",
                expected.clone(),
            );
            check_path_selection(
                spec,
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
                    selections: vec![NamedSelection::field(
                        Some(Alias::new("leggo")),
                        Key::quoted("my ego").into_with_range(),
                        None,
                    )],
                    ..Default::default()
                }),
            );

            check_path_selection(
                spec,
                "nested.'string literal' { leggo: 'my ego' }",
                expected.clone(),
            );

            check_path_selection(
                spec,
                " nested . 'string literal' { leggo : 'my ego' } ",
                expected.clone(),
            );

            check_path_selection(
                spec,
                "nested. 'string literal' { leggo: 'my ego' }",
                expected.clone(),
            );

            check_path_selection(
                spec,
                "nested . 'string literal' { leggo: 'my ego' }",
                expected.clone(),
            );
            check_path_selection(
                spec,
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
                            selections: vec![NamedSelection::field(
                                None,
                                Key::quoted("quoted without alias").into_with_range(),
                                Some(SubSelection {
                                    selections: vec![
                                        NamedSelection::field(
                                            None,
                                            Key::field("id").into_with_range(),
                                            None,
                                        ),
                                        NamedSelection::field(
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
                spec,
                "$.results{'quoted without alias'{id'n a m e'}}",
                expected.clone(),
            );
            check_path_selection(
                spec,
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
                            selections: vec![NamedSelection::field(
                                Some(Alias::quoted("non-identifier alias")),
                                Key::quoted("quoted with alias").into_with_range(),
                                Some(SubSelection {
                                    selections: vec![
                                        NamedSelection::field(
                                            None,
                                            Key::field("id").into_with_range(),
                                            None,
                                        ),
                                        NamedSelection::field(
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
                spec,
                "$.results{'non-identifier alias':'quoted with alias'{id'n a m e':name}}",
                expected.clone(),
            );
            check_path_selection(
                spec,
                " $ . results { 'non-identifier alias' : 'quoted with alias' { id 'n a m e': name } } ",
                expected,
            );
        }
    }

    #[rstest]
    #[case::v0_2(ConnectSpec::V0_2)]
    #[case::v0_3(ConnectSpec::V0_3)]
    fn test_path_selection_vars(#[case] spec: ConnectSpec) {
        check_path_selection(
            spec,
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
            spec,
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
            spec,
            "$this { hello }",
            PathSelection {
                path: PathList::Var(
                    KnownVariable::External(Namespace::This.to_string()).into_with_range(),
                    PathList::Selection(SubSelection {
                        selections: vec![NamedSelection::field(
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
            spec,
            "$ { hello }",
            PathSelection {
                path: PathList::Var(
                    KnownVariable::Dollar.into_with_range(),
                    PathList::Selection(SubSelection {
                        selections: vec![NamedSelection::field(
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
            spec,
            "$this { before alias: $args.arg after }",
            PathList::Var(
                KnownVariable::External(Namespace::This.to_string()).into_with_range(),
                PathList::Selection(SubSelection {
                    selections: vec![
                        NamedSelection::field(None, Key::field("before").into_with_range(), None),
                        NamedSelection {
                            prefix: NamingPrefix::Alias(Alias::new("alias")),
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
                        NamedSelection::field(None, Key::field("after").into_with_range(), None),
                    ],
                    ..Default::default()
                })
                .into_with_range(),
            )
            .into(),
        );

        check_path_selection(
            spec,
            "$.nested { key injected: $args.arg }",
            PathSelection {
                path: PathList::Var(
                    KnownVariable::Dollar.into_with_range(),
                    PathList::Key(
                        Key::field("nested").into_with_range(),
                        PathList::Selection(SubSelection {
                            selections: vec![
                                NamedSelection::field(
                                    None,
                                    Key::field("key").into_with_range(),
                                    None,
                                ),
                                NamedSelection {
                                    prefix: NamingPrefix::Alias(Alias::new("injected")),
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
            spec,
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
            spec,
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
            spec,
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
            spec,
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
        fn check_path_parse_error(
            input: &str,
            expected_offset: usize,
            expected_message: impl Into<String>,
        ) {
            let expected_message: String = expected_message.into();
            match PathSelection::parse(new_span_with_spec(input, ConnectSpec::latest())) {
                Ok((remainder, path)) => {
                    panic!(
                        "Expected error at offset {expected_offset} with message '{expected_message}', but got path {path:?} and remainder {remainder:?}",
                    );
                }
                Err(nom::Err::Error(e) | nom::Err::Failure(e)) => {
                    assert_eq!(&input[expected_offset..], *e.input.fragment());
                    // The PartialEq implementation for LocatedSpan
                    // unfortunately ignores span.extra, so we have to check
                    // e.input.extra manually.
                    assert_eq!(
                        e.input.extra,
                        SpanExtra {
                            spec: ConnectSpec::latest(),
                            errors: vec![(expected_message, expected_offset)],
                            local_vars: Vec::new(),
                        }
                    );
                }
                Err(e) => {
                    panic!("Unexpected error {e:?}");
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
            JSONSelection::path(PathSelection {
                path: PathList::Var(
                    KnownVariable::Dollar.into_with_range(),
                    PathList::Empty.into_with_range()
                )
                .into_with_range(),
            }),
        );

        assert_eq!(
            selection!("$this").strip_ranges(),
            JSONSelection::path(PathSelection {
                path: PathList::Var(
                    KnownVariable::External(Namespace::This.to_string()).into_with_range(),
                    PathList::Empty.into_with_range()
                )
                .into_with_range(),
            }),
        );

        assert_eq!(
            selection!("value: $ a { b c }").strip_ranges(),
            JSONSelection::named(SubSelection {
                selections: vec![
                    NamedSelection {
                        prefix: NamingPrefix::Alias(Alias::new("value")),
                        path: PathSelection {
                            path: PathList::Var(
                                KnownVariable::Dollar.into_with_range(),
                                PathList::Empty.into_with_range()
                            )
                            .into_with_range(),
                        },
                    },
                    NamedSelection::field(
                        None,
                        Key::field("a").into_with_range(),
                        Some(SubSelection {
                            selections: vec![
                                NamedSelection::field(
                                    None,
                                    Key::field("b").into_with_range(),
                                    None
                                ),
                                NamedSelection::field(
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
            JSONSelection::named(SubSelection {
                selections: vec![NamedSelection {
                    prefix: NamingPrefix::Alias(Alias::new("value")),
                    path: PathSelection {
                        path: PathList::Var(
                            KnownVariable::External(Namespace::This.to_string()).into_with_range(),
                            PathList::Selection(SubSelection {
                                selections: vec![
                                    NamedSelection::field(
                                        None,
                                        Key::field("b").into_with_range(),
                                        None
                                    ),
                                    NamedSelection::field(
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
    fn test_error_snapshots_v0_2() {
        let spec = ConnectSpec::V0_2;

        // The .data shorthand is no longer allowed, since it can be mistakenly
        // parsed as a continuation of a previous selection. Instead, use $.data
        // to achieve the same effect without ambiguity.
        assert_debug_snapshot!(JSONSelection::parse_with_spec(".data", spec));

        // If you want to mix a path selection with other named selections, the
        // path selection must have a trailing subselection, to enforce that it
        // returns an object with statically known keys, or be inlined/spread
        // with a ... token.
        assert_debug_snapshot!(JSONSelection::parse_with_spec("id $.object", spec));
    }

    #[test]
    fn test_error_snapshots_v0_3() {
        let spec = ConnectSpec::V0_3;

        // When this assertion fails, don't panic, but it's time to decide how
        // the next-next version should behave in these error cases (possibly
        // exactly the same).
        assert_eq!(spec, ConnectSpec::next());

        // The .data shorthand is no longer allowed, since it can be mistakenly
        // parsed as a continuation of a previous selection. Instead, use $.data
        // to achieve the same effect without ambiguity.
        assert_debug_snapshot!(JSONSelection::parse_with_spec(".data", spec));

        // If you want to mix a path selection with other named selections, the
        // path selection must have a trailing subselection, to enforce that it
        // returns an object with statically known keys, or be inlined/spread
        // with a ... token.
        assert_debug_snapshot!(JSONSelection::parse_with_spec("id $.object", spec));
    }

    #[rstest]
    #[case::v0_2(ConnectSpec::V0_2)]
    #[case::v0_3(ConnectSpec::V0_3)]
    fn test_path_selection_at(#[case] spec: ConnectSpec) {
        check_path_selection(
            spec,
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
            spec,
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
            spec,
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

    #[rstest]
    #[case::v0_2(ConnectSpec::V0_2)]
    #[case::v0_3(ConnectSpec::V0_3)]
    fn test_expr_path_selections(#[case] spec: ConnectSpec) {
        fn check_simple_lit_expr(spec: ConnectSpec, input: &str, expected: LitExpr) {
            check_path_selection(
                spec,
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

        check_simple_lit_expr(spec, "$(null)", LitExpr::Null);

        check_simple_lit_expr(spec, "$(true)", LitExpr::Bool(true));
        check_simple_lit_expr(spec, "$(false)", LitExpr::Bool(false));

        check_simple_lit_expr(
            spec,
            "$(1234)",
            LitExpr::Number("1234".parse().expect("serde_json::Number parse error")),
        );
        check_simple_lit_expr(
            spec,
            "$(1234.5678)",
            LitExpr::Number("1234.5678".parse().expect("serde_json::Number parse error")),
        );

        check_simple_lit_expr(
            spec,
            "$('hello world')",
            LitExpr::String("hello world".to_string()),
        );
        check_simple_lit_expr(
            spec,
            "$(\"hello world\")",
            LitExpr::String("hello world".to_string()),
        );
        check_simple_lit_expr(
            spec,
            "$(\"hello \\\"world\\\"\")",
            LitExpr::String("hello \"world\"".to_string()),
        );

        check_simple_lit_expr(
            spec,
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

        check_simple_lit_expr(spec, "$({})", LitExpr::Object(IndexMap::default()));
        check_simple_lit_expr(
            spec,
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
    }

    #[test]
    fn test_path_expr_with_spaces_v0_2() {
        assert_debug_snapshot!(selection!(
            " suffix : results -> slice ( $( - 1 ) -> mul ( $args . suffixLength ) ) ",
            // Snapshot tests can be brittle when used with (multiple) #[rstest]
            // cases, since the filenames of the snapshots do not always take
            // into account the differences between the cases, so we hard-code
            // the ConnectSpec in tests like this.
            ConnectSpec::V0_2
        ));
    }

    #[test]
    fn test_path_expr_with_spaces_v0_3() {
        assert_debug_snapshot!(selection!(
            " suffix : results -> slice ( $( - 1 ) -> mul ( $args . suffixLength ) ) ",
            ConnectSpec::V0_3
        ));
    }

    #[rstest]
    #[case::v0_2(ConnectSpec::V0_2)]
    #[case::v0_3(ConnectSpec::V0_3)]
    fn test_path_methods(#[case] spec: ConnectSpec) {
        check_path_selection(
            spec,
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
            check_path_selection(spec, "data->query($.a, $.b, $.c)", expected.clone());
            check_path_selection(spec, "data->query($.a, $.b, $.c )", expected.clone());
            check_path_selection(spec, "data->query($.a, $.b, $.c,)", expected.clone());
            check_path_selection(spec, "data->query($.a, $.b, $.c ,)", expected.clone());
            check_path_selection(spec, "data->query($.a, $.b, $.c , )", expected);
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
            check_path_selection(spec, "data.x->concat([data.y, data.z])", expected.clone());
            check_path_selection(spec, "data.x->concat([ data.y, data.z ])", expected.clone());
            check_path_selection(spec, "data.x->concat([data.y, data.z,])", expected.clone());
            check_path_selection(
                spec,
                "data.x->concat([data.y, data.z , ])",
                expected.clone(),
            );
            check_path_selection(spec, "data.x->concat([data.y, data.z,],)", expected.clone());
            check_path_selection(spec, "data.x->concat([data.y, data.z , ] , )", expected);
        }

        check_path_selection(
            spec,
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
                                                selections: vec![NamedSelection {
                                                    prefix: NamingPrefix::Alias(Alias::new("x2")),
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
                                                selections: vec![NamedSelection {
                                                    prefix: NamingPrefix::Alias(Alias::new("y2")),
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
                span_is_all_spaces_or_comments(remainder.clone()),
                "remainder is `{:?}`",
                remainder.clone(),
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
                selections: vec![NamedSelection::field(
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
                selections: vec![NamedSelection::field(
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
                selections: vec![NamedSelection::field(
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
                    NamedSelection::field(None, Key::field("hello").into_with_range(), None),
                    NamedSelection::field(None, Key::field("world").into_with_range(), None),
                ],
                ..Default::default()
            },
        );

        check_parsed(
            "{ hello { world } }",
            SubSelection {
                selections: vec![NamedSelection::field(
                    None,
                    Key::field("hello").into_with_range(),
                    Some(SubSelection {
                        selections: vec![NamedSelection::field(
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
            let this_kind_path = match &sel.inner {
                TopLevelSelection::Path(path) => path,
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
    fn test_local_var_paths() {
        let spec = ConnectSpec::V0_3;
        let name_selection = selection!(
            "person->as($name, @.name)->as($stray, 123)->echo({ hello: $name })",
            spec
        );
        let local_var_names = name_selection.local_var_names();
        assert_eq!(local_var_names.len(), 2);
        assert!(local_var_names.contains("$name"));
        assert!(local_var_names.contains("$stray"));
    }

    #[test]
    fn test_ranged_locations() {
        fn check(input: &str, expected: JSONSelection) {
            let parsed = JSONSelection::parse(input).unwrap();
            assert_eq!(parsed, expected);
        }

        check(
            "hello",
            JSONSelection::named(SubSelection {
                selections: vec![NamedSelection::field(
                    None,
                    WithRange::new(Key::field("hello"), Some(0..5)),
                    None,
                )],
                range: Some(0..5),
            }),
        );

        check(
            "  hello ",
            JSONSelection::named(SubSelection {
                selections: vec![NamedSelection::field(
                    None,
                    WithRange::new(Key::field("hello"), Some(2..7)),
                    None,
                )],
                range: Some(2..7),
            }),
        );

        check(
            "  hello  { hi name }",
            JSONSelection::named(SubSelection {
                selections: vec![NamedSelection::field(
                    None,
                    WithRange::new(Key::field("hello"), Some(2..7)),
                    Some(SubSelection {
                        selections: vec![
                            NamedSelection::field(
                                None,
                                WithRange::new(Key::field("hi"), Some(11..13)),
                                None,
                            ),
                            NamedSelection::field(
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
            JSONSelection::path(PathSelection {
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
            JSONSelection::path(PathSelection {
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
            JSONSelection::named(SubSelection {
                selections: vec![
                    NamedSelection::field(
                        None,
                        WithRange::new(Key::field("before"), Some(0..6)),
                        None,
                    ),
                    NamedSelection {
                        prefix: NamingPrefix::Alias(Alias {
                            name: WithRange::new(Key::field("product"), Some(7..14)),
                            range: Some(7..15),
                        }),
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
                                                        NamedSelection::field(
                                                            None,
                                                            WithRange::new(
                                                                Key::field("id"),
                                                                Some(29..31),
                                                            ),
                                                            None,
                                                        ),
                                                        NamedSelection::field(
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
                    NamedSelection::field(
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
        let spec = ConnectSpec::V0_2;

        let selection_null_stringify_v0_2 = selection!("$(null->jsonStringify)", spec);
        assert_eq!(
            selection_null_stringify_v0_2.pretty_print(),
            "$(null->jsonStringify)"
        );

        let selection_hello_slice_v0_2 = selection!("sliced: $('hello'->slice(1, 3))", spec);
        assert_eq!(
            selection_hello_slice_v0_2.pretty_print(),
            "sliced: $(\"hello\"->slice(1, 3))"
        );

        let selection_true_not_v0_2 = selection!("true->not", spec);
        assert_eq!(selection_true_not_v0_2.pretty_print(), "true->not");

        let selection_false_not_v0_2 = selection!("false->not", spec);
        assert_eq!(selection_false_not_v0_2.pretty_print(), "false->not");

        let selection_object_path_v0_2 = selection!("$({ a: 123 }.a)", spec);
        assert_eq!(
            selection_object_path_v0_2.pretty_print_with_indentation(true, 0),
            "$({ a: 123 }.a)"
        );

        let selection_array_path_v0_2 = selection!("$([1, 2, 3]->get(1))", spec);
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
        let spec = ConnectSpec::V0_3;

        check_path_selection(
            spec,
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
    fn test_unambiguous_single_key_paths_v0_2() {
        let spec = ConnectSpec::V0_2;

        let mul_with_dollars = selection!("a->mul($.b, $.c)", spec);
        mul_with_dollars.if_named_else_path(
            |named| {
                panic!("Expected a path selection, got named: {named:?}");
            },
            |path| {
                assert_eq!(path.get_single_key(), None);
                assert_eq!(path.pretty_print(), "a->mul($.b, $.c)");
            },
        );

        assert_debug_snapshot!(mul_with_dollars);
    }

    #[test]
    fn test_invalid_single_key_paths_v0_2() {
        let spec = ConnectSpec::V0_2;

        let a_plus_b_plus_c = JSONSelection::parse_with_spec("a->add(b, c)", spec);
        assert_eq!(a_plus_b_plus_c, Err(JSONSelectionParseError {
            message: "Named path selection must either begin with alias or ..., or end with subselection".to_string(),
            fragment: "a->add(b, c)".to_string(),
            offset: 0,
            spec: ConnectSpec::V0_2,
        }));

        let sum_a_plus_b_plus_c = JSONSelection::parse_with_spec("sum: a->add(b, c)", spec);
        assert_eq!(
            sum_a_plus_b_plus_c,
            Err(JSONSelectionParseError {
                message: "nom::error::ErrorKind::Eof".to_string(),
                fragment: "(b, c)".to_string(),
                offset: 11,
                spec: ConnectSpec::V0_2,
            })
        );
    }

    #[test]
    fn test_optional_method_call() {
        let spec = ConnectSpec::V0_3;

        check_path_selection(
            spec,
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
        let spec = ConnectSpec::V0_3;

        check_path_selection(
            spec,
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
        let spec = ConnectSpec::V0_3;

        check_path_selection(
            spec,
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
    fn test_invalid_sequential_question_marks() {
        let spec = ConnectSpec::V0_3;

        assert_eq!(
            JSONSelection::parse_with_spec("baz: $.foo??.bar", spec),
            Err(JSONSelectionParseError {
                message: "nom::error::ErrorKind::Eof".to_string(),
                fragment: "??.bar".to_string(),
                offset: 10,
                spec,
            }),
        );

        assert_eq!(
            JSONSelection::parse_with_spec("baz: $.foo?->echo(null)??.bar", spec),
            Err(JSONSelectionParseError {
                message: "nom::error::ErrorKind::Eof".to_string(),
                fragment: "??.bar".to_string(),
                offset: 23,
                spec,
            }),
        );
    }

    #[test]
    fn test_invalid_infix_operator_parsing() {
        let spec = ConnectSpec::V0_2;

        assert_eq!(
            JSONSelection::parse_with_spec("aOrB: $($.a ?? $.b)", spec),
            Err(JSONSelectionParseError {
                message: "nom::error::ErrorKind::Eof".to_string(),
                fragment: "($.a ?? $.b)".to_string(),
                offset: 7,
                spec,
            }),
        );

        assert_eq!(
            JSONSelection::parse_with_spec("aOrB: $($.a ?! $.b)", spec),
            Err(JSONSelectionParseError {
                message: "nom::error::ErrorKind::Eof".to_string(),
                fragment: "($.a ?! $.b)".to_string(),
                offset: 7,
                spec,
            }),
        );
    }

    #[test]
    fn test_optional_chaining_with_subselection() {
        let spec = ConnectSpec::V0_3;

        check_path_selection(
            spec,
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
                                        NamedSelection::field(
                                            None,
                                            Key::field("id").into_with_range(),
                                            None,
                                        ),
                                        NamedSelection::field(
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
        let spec = ConnectSpec::V0_3;

        check_path_selection(
            spec,
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

    #[test]
    fn test_unambiguous_single_key_paths_v0_3() {
        let spec = ConnectSpec::V0_3;

        let mul_with_dollars = selection!("a->mul($.b, $.c)", spec);
        mul_with_dollars.if_named_else_path(
            |named| {
                panic!("Expected a path selection, got named: {named:?}");
            },
            |path| {
                assert_eq!(path.get_single_key(), None);
                assert_eq!(path.pretty_print(), "a->mul($.b, $.c)");
            },
        );

        assert_debug_snapshot!(mul_with_dollars);
    }

    #[test]
    fn test_valid_single_key_path_v0_3() {
        let spec = ConnectSpec::V0_3;

        let a_plus_b_plus_c = JSONSelection::parse_with_spec("a->add(b, c)", spec);
        if let Ok(selection) = a_plus_b_plus_c {
            selection.if_named_else_path(
                |named| {
                    panic!("Expected a path selection, got named: {named:?}");
                },
                |path| {
                    assert_eq!(path.pretty_print(), "a->add(b, c)");
                    assert_eq!(path.get_single_key(), None);
                },
            );
            assert_debug_snapshot!(selection);
        } else {
            panic!("Expected a valid selection, got error: {a_plus_b_plus_c:?}");
        }
    }

    #[test]
    fn test_valid_single_key_path_with_alias_v0_3() {
        let spec = ConnectSpec::V0_3;

        let sum_a_plus_b_plus_c = JSONSelection::parse_with_spec("sum: a->add(b, c)", spec);
        if let Ok(selection) = sum_a_plus_b_plus_c {
            selection.if_named_else_path(
                |named| {
                    for selection in named.selections_iter() {
                        assert_eq!(selection.pretty_print(), "sum: a->add(b, c)");
                        assert_eq!(
                            selection.get_single_key().map(|key| key.as_str()),
                            Some("sum")
                        );
                    }
                },
                |path| {
                    panic!("Expected any number of named selections, got path: {path:?}");
                },
            );
            assert_debug_snapshot!(selection);
        } else {
            panic!("Expected a valid selection, got error: {sum_a_plus_b_plus_c:?}");
        }
    }

    #[test]
    fn test_disallowed_spread_syntax_error() {
        assert_eq!(
            JSONSelection::parse_with_spec("id ...names", ConnectSpec::V0_2),
            Err(JSONSelectionParseError {
                message: "nom::error::ErrorKind::Eof".to_string(),
                fragment: "...names".to_string(),
                offset: 3,
                spec: ConnectSpec::V0_2,
            }),
        );

        assert_eq!(
            JSONSelection::parse_with_spec("id ...names", ConnectSpec::V0_3),
            Err(JSONSelectionParseError {
                message: "Spread syntax (...) is planned for connect/v0.4".to_string(),
                // This is the fragment and offset we should get, but we need to
                // store error offsets in SpanExtra::errors to provide that
                // information.
                fragment: "...names".to_string(),
                offset: 3,
                spec: ConnectSpec::V0_3,
            }),
        );

        // This will fail when we promote v0.3 to latest and create v0.4, which
        // is your signal to consider reenabling the tests below.
        assert_eq!(ConnectSpec::V0_3, ConnectSpec::next());
    }

    // TODO Reenable these tests in ConnectSpec::V0_4 when we support ... spread
    // syntax and abstract types.
    /** #[cfg(test)]
    mod spread_parsing {
        use crate::connectors::ConnectSpec;
        use crate::connectors::json_selection::PrettyPrintable;
        use crate::selection;

        #[track_caller]
        pub(super) fn check(spec: ConnectSpec, input: &str, expected_pretty: &str) {
            let selection = selection!(input, spec);
            assert_eq!(selection.pretty_print(), expected_pretty);
        }
    }

    #[test]
    fn test_basic_spread_parsing_one_field() {
        let spec = ConnectSpec::V0_4;
        let expected = "... a";
        spread_parsing::check(spec, "...a", expected);
        spread_parsing::check(spec, "... a", expected);
        spread_parsing::check(spec, "...a ", expected);
        spread_parsing::check(spec, "... a ", expected);
        spread_parsing::check(spec, " ... a ", expected);
        spread_parsing::check(spec, "...\na", expected);
        assert_debug_snapshot!(selection!("...a", spec));
    }

    #[test]
    fn test_spread_parsing_spread_a_spread_b() {
        let spec = ConnectSpec::V0_4;
        let expected = "... a\n... b";
        spread_parsing::check(spec, "...a...b", expected);
        spread_parsing::check(spec, "... a ... b", expected);
        spread_parsing::check(spec, "... a ...b", expected);
        spread_parsing::check(spec, "... a ... b ", expected);
        spread_parsing::check(spec, " ... a ... b ", expected);
        assert_debug_snapshot!(selection!("...a...b", spec));
    }

    #[test]
    fn test_spread_parsing_a_spread_b() {
        let spec = ConnectSpec::V0_4;
        let expected = "a\n... b";
        spread_parsing::check(spec, "a...b", expected);
        spread_parsing::check(spec, "a ... b", expected);
        spread_parsing::check(spec, "a\n...b", expected);
        spread_parsing::check(spec, "a\n...\nb", expected);
        spread_parsing::check(spec, "a...\nb", expected);
        spread_parsing::check(spec, " a ... b", expected);
        spread_parsing::check(spec, " a ...b", expected);
        spread_parsing::check(spec, " a ... b ", expected);
        assert_debug_snapshot!(selection!("a...b", spec));
    }

    #[test]
    fn test_spread_parsing_spread_a_b() {
        let spec = ConnectSpec::V0_4;
        let expected = "... a\nb";
        spread_parsing::check(spec, "...a b", expected);
        spread_parsing::check(spec, "... a b", expected);
        spread_parsing::check(spec, "... a b ", expected);
        spread_parsing::check(spec, "... a\nb", expected);
        spread_parsing::check(spec, "... a\n b", expected);
        spread_parsing::check(spec, " ... a b ", expected);
        assert_debug_snapshot!(selection!("...a b", spec));
    }

    #[test]
    fn test_spread_parsing_spread_a_b_c() {
        let spec = ConnectSpec::V0_4;
        let expected = "... a\nb\nc";
        spread_parsing::check(spec, "...a b c", expected);
        spread_parsing::check(spec, "... a b c", expected);
        spread_parsing::check(spec, "... a b c ", expected);
        spread_parsing::check(spec, "... a\nb\nc", expected);
        spread_parsing::check(spec, "... a\nb\n c", expected);
        spread_parsing::check(spec, " ... a b c ", expected);
        spread_parsing::check(spec, "...\na b c", expected);
        assert_debug_snapshot!(selection!("...a b c", spec));
    }

    #[test]
    fn test_spread_parsing_spread_spread_a_sub_b() {
        let spec = ConnectSpec::V0_4;
        let expected = "... a {\n  b\n}";
        spread_parsing::check(spec, "...a{b}", expected);
        spread_parsing::check(spec, "... a { b }", expected);
        spread_parsing::check(spec, "...a { b }", expected);
        spread_parsing::check(spec, "... a { b } ", expected);
        spread_parsing::check(spec, "... a\n{ b }", expected);
        spread_parsing::check(spec, "... a\n{b}", expected);
        spread_parsing::check(spec, " ... a { b } ", expected);
        spread_parsing::check(spec, "...\na { b }", expected);
        assert_debug_snapshot!(selection!("...a{b}", spec));
    }

    #[test]
    fn test_spread_parsing_spread_a_sub_b_c() {
        let spec = ConnectSpec::V0_4;
        let expected = "... a {\n  b\n  c\n}";
        spread_parsing::check(spec, "...a{b c}", expected);
        spread_parsing::check(spec, "... a { b c }", expected);
        spread_parsing::check(spec, "...a { b c }", expected);
        spread_parsing::check(spec, "... a { b c } ", expected);
        spread_parsing::check(spec, "... a\n{ b c }", expected);
        spread_parsing::check(spec, "... a\n{b c}", expected);
        spread_parsing::check(spec, " ... a { b c } ", expected);
        spread_parsing::check(spec, "...\na { b c }", expected);
        spread_parsing::check(spec, "...\na { b\nc }", expected);
        assert_debug_snapshot!(selection!("...a{b c}", spec));
    }

    #[test]
    fn test_spread_parsing_spread_a_sub_b_spread_c() {
        let spec = ConnectSpec::V0_4;
        let expected = "... a {\n  b\n  ... c\n}";
        spread_parsing::check(spec, "...a{b...c}", expected);
        spread_parsing::check(spec, "... a { b ... c }", expected);
        spread_parsing::check(spec, "...a { b ... c }", expected);
        spread_parsing::check(spec, "... a { b ... c } ", expected);
        spread_parsing::check(spec, "... a\n{ b ... c }", expected);
        spread_parsing::check(spec, "... a\n{b ... c}", expected);
        spread_parsing::check(spec, " ... a { b ... c } ", expected);
        spread_parsing::check(spec, "...\na { b ... c }", expected);
        spread_parsing::check(spec, "...\na {b ...\nc }", expected);
        assert_debug_snapshot!(selection!("...a{b...c}", spec));
    }

    #[test]
    fn test_spread_parsing_spread_a_sub_b_spread_c_d() {
        let spec = ConnectSpec::V0_4;
        let expected = "... a {\n  b\n  ... c\n  d\n}";
        spread_parsing::check(spec, "...a{b...c d}", expected);
        spread_parsing::check(spec, "... a { b ... c d }", expected);
        spread_parsing::check(spec, "...a { b ... c d }", expected);
        spread_parsing::check(spec, "... a { b ... c d } ", expected);
        spread_parsing::check(spec, "... a\n{ b ... c d }", expected);
        spread_parsing::check(spec, "... a\n{b ... c d}", expected);
        spread_parsing::check(spec, " ... a { b ... c d } ", expected);
        spread_parsing::check(spec, "...\na { b ... c d }", expected);
        spread_parsing::check(spec, "...\na {b ...\nc d }", expected);
        assert_debug_snapshot!(selection!("...a{b...c d}", spec));
    }

    #[test]
    fn test_spread_parsing_spread_a_sub_spread_b_c_d_spread_e() {
        let spec = ConnectSpec::V0_4;
        let expected = "... a {\n  ... b\n  c\n  d\n  ... e\n}";
        spread_parsing::check(spec, "...a{...b c d...e}", expected);
        spread_parsing::check(spec, "... a { ... b c d ... e }", expected);
        spread_parsing::check(spec, "...a { ... b c d ... e }", expected);
        spread_parsing::check(spec, "... a { ... b c d ... e } ", expected);
        spread_parsing::check(spec, "... a\n{ ... b c d ... e }", expected);
        spread_parsing::check(spec, "... a\n{... b c d ... e}", expected);
        spread_parsing::check(spec, " ... a { ... b c d ... e } ", expected);
        spread_parsing::check(spec, "...\na { ... b c d ... e }", expected);
        spread_parsing::check(spec, "...\na {...\nb\nc d ...\ne }", expected);
        assert_debug_snapshot!(selection!("...a{...b c d...e}", spec));
    }
    **/

    #[test]
    fn should_parse_null_coalescing_in_connect_0_3() {
        assert!(JSONSelection::parse_with_spec("sum: $(a ?? b)", ConnectSpec::V0_3).is_ok());
        assert!(JSONSelection::parse_with_spec("sum: $(a ?! b)", ConnectSpec::V0_3).is_ok());
    }

    #[test]
    fn should_not_parse_null_coalescing_in_connect_0_2() {
        assert!(JSONSelection::parse_with_spec("sum: $(a ?? b)", ConnectSpec::V0_2).is_err());
        assert!(JSONSelection::parse_with_spec("sum: $(a ?! b)", ConnectSpec::V0_2).is_err());
    }

    #[test]
    fn should_not_parse_mixed_operators_in_same_expression() {
        let result = JSONSelection::parse_with_spec("sum: $(a ?? b ?! c)", ConnectSpec::V0_3);

        let err = result.expect_err("Expected parse error for mixed operators ?? and ?!");
        assert_eq!(
            err.message,
            "Found mixed operators ?? and ?!. You can only chain operators of the same kind."
        );

        // Also test the reverse order
        let result2 = JSONSelection::parse_with_spec("sum: $(a ?! b ?? c)", ConnectSpec::V0_3);
        let err2 = result2.expect_err("Expected parse error for mixed operators ?! and ??");
        assert_eq!(
            err2.message,
            "Found mixed operators ?! and ??. You can only chain operators of the same kind."
        );
    }

    #[test]
    fn should_parse_mixed_operators_in_nested_expression() {
        let result = JSONSelection::parse_with_spec("sum: $(a ?? $(b ?! c))", ConnectSpec::V0_3);

        assert!(result.is_ok());
    }

    #[test]
    fn should_parse_local_vars_as_such() {
        let spec = ConnectSpec::V0_3;
        // No external variable references because $ and @ are internal, and
        // $root is locally bound by the ->as method everywhere it's used.
        let all_local = selection!("$->as($root, @.data)->echo([$root, $root])", spec);
        assert!(all_local.external_var_paths().is_empty());
        assert_debug_snapshot!(all_local);

        // Introducing one external variable reference: $ext.
        let ext = selection!("$->as($root, @.data)->echo([$root, $ext])", spec);
        let external_vars = ext.external_var_paths();
        assert_eq!(external_vars.len(), 1);

        for ext_var in &external_vars {
            match ext_var.path.as_ref() {
                PathList::Var(var, _) => match var.as_ref() {
                    KnownVariable::External(var_name) => {
                        assert_eq!(var_name, "$ext");
                    }
                    _ => panic!("Expected external variable, got: {var:?}"),
                },
                _ => panic!(
                    "Expected variable at start of path, got: {:?}",
                    &ext_var.path
                ),
            };
        }

        assert_debug_snapshot!(ext);
    }
}
