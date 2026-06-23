//! Structural comparison of two [`JSONSelection`] ASTs, designed for
//! surveying how the same source text parses differently across
//! [`ConnectSpec`](crate::connectors::ConnectSpec) versions.
//!
//! The main entry point is [`JSONSelection::diff_kinds`], which returns
//! a [`Vec<DiffKind>`] classifying each structural divergence found
//! between two parses. The classifier is tuned for the v0.3→v0.4
//! grammar shift introduced by the `SubSelection`/`LitObject` unification:
//!
//! - The **breaking** class is "primitive-token-in-value-position" — a
//!   bare `true`/`false`/`null` or quoted string that v0.3 parsed as a
//!   field reference but v0.4 parses as a JSON literal. These show up
//!   as [`DiffKind::KeyFlippedToLiteralNull`], `Bool`, and `String`.
//!
//! - The **cosmetic** class is the unification itself: v0.3's
//!   `Alias { … }` was parsed as a `PathSelection` whose only path
//!   element was the trailing `SubSelection`; v0.4 parses the same
//!   source as `LitExpr::Object(SubSelection)`. Same evaluation
//!   semantics, different AST shape. These show up as
//!   [`DiffKind::SubSelectionToLitObject`].
//!
//! Anything we can't classify is [`DiffKind::Other`].

use serde::Serialize;

use crate::connectors::json_selection::JSONSelection;
use crate::connectors::json_selection::Key;
use crate::connectors::json_selection::LitExpr;
use crate::connectors::json_selection::NamedSelection;
use crate::connectors::json_selection::PathList;
use crate::connectors::json_selection::PathSelection;
use crate::connectors::json_selection::SubSelection;
use crate::connectors::json_selection::TopLevelSelection;
use crate::connectors::json_selection::location::OffsetRange;
use crate::connectors::json_selection::location::Ranged;
use crate::connectors::json_selection::location::WithRange;

/// A single structural difference between two [`JSONSelection`] parses
/// of the same source text under different
/// [`ConnectSpec`](crate::connectors::ConnectSpec) versions.
///
/// The breaking variants (`KeyFlippedTo…`) carry a `followed_by` field
/// that records whether the v0.4 literal is followed by additional
/// path access (e.g., `null.foo`, `"x"->method`, `true { … }`). When
/// `followed_by != FollowedBy::Nothing` the v0.4 form is almost
/// certainly a parsing accident — literals don't have fields — and
/// the user's clear intent was the v0.3 field-reference reading. Such
/// sites can be auto-fixed by prepending `$.` to the original token.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DiffKind {
    /// A bare `null` token that v0.3 parsed as the field reference
    /// `Key::Field("null")` but v0.4 parses as `LitExpr::Null`.
    KeyFlippedToLiteralNull {
        source_range: Option<(usize, usize)>,
        followed_by: FollowedBy,
    },

    /// A bare `true` or `false` token that v0.3 parsed as the field
    /// reference `Key::Field("true" | "false")` but v0.4 parses as
    /// `LitExpr::Bool(value)`.
    KeyFlippedToLiteralBool {
        value: bool,
        source_range: Option<(usize, usize)>,
        followed_by: FollowedBy,
    },

    /// A bare identifier-shaped token that v0.3 parsed as a field
    /// reference but v0.4 parses as a string literal. (This is rare;
    /// usually `Key::Field` → `LitExpr::Bool/Null` is the case.)
    KeyFieldFlippedToLiteralString {
        text: String,
        source_range: Option<(usize, usize)>,
        followed_by: FollowedBy,
    },

    /// A quoted-string token that v0.3 parsed as the field reference
    /// `Key::Quoted("...")` but v0.4 parses as `LitExpr::String("...")`.
    KeyQuotedFlippedToLiteralString {
        text: String,
        source_range: Option<(usize, usize)>,
        followed_by: FollowedBy,
    },

    /// v0.3 parsed `Alias { ... }` as a `PathSelection` whose only
    /// path element was a trailing `SubSelection`; v0.4 parses the
    /// same source as `LitExpr::Object(SubSelection)`. Semantically
    /// equivalent — emitted so the survey can distinguish cosmetic
    /// unification noise from real breaking changes.
    SubSelectionToLitObject {
        source_range: Option<(usize, usize)>,
    },

    /// v0.3 parsed an object literal as `LegacyObject` (key → value
    /// map); v0.4 parses it as `LitExpr::Object(SubSelection)`.
    /// Cosmetic — same evaluation semantics under the unification.
    LegacyObjectToLitObject {
        source_range: Option<(usize, usize)>,
    },

    /// A divergence we don't classify in detail. Carries the AST
    /// variant names from each parse for triage.
    Other {
        v03_variant: &'static str,
        v04_variant: &'static str,
        source_range: Option<(usize, usize)>,
    },
}

/// What (if anything) follows a v0.4 literal in the `LitPath` tail.
/// Literals don't currently carry fields/methods of their own, so any
/// non-`Nothing` value here strongly suggests the user's intent was a
/// field-reference (the v0.3 reading), and the v0.4 parse is a
/// silently-accepted accident.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FollowedBy {
    /// Nothing — the literal stands alone (e.g., `foo: null`).
    Nothing,
    /// Path access via `.key` or `."quoted"`.
    KeyAccess,
    /// Method invocation via `->method(...)`.
    Method,
    /// A trailing sub-selection block `{ ... }`.
    SubSelection,
    /// Optional-chaining `?`.
    Question,
    /// A v0.4-`$(...)` expression follow-on (shouldn't normally happen).
    Expr,
}

fn classify_followed_by(tail: &WithRange<PathList>) -> FollowedBy {
    match tail.as_ref() {
        PathList::Empty => FollowedBy::Nothing,
        PathList::Key(_, _) => FollowedBy::KeyAccess,
        PathList::Method(_, _, _) => FollowedBy::Method,
        PathList::Selection(_) => FollowedBy::SubSelection,
        PathList::Question(_) => FollowedBy::Question,
        PathList::Expr(_, _) => FollowedBy::Expr,
        PathList::Var(_, _) => FollowedBy::KeyAccess,
    }
}

impl JSONSelection {
    /// Walk the inner AST of `self` and `other` in lockstep and return
    /// a list of classified structural differences. The returned `Vec`
    /// is empty iff [`structural_eq`](Self::structural_eq) returns
    /// `true`.
    ///
    /// This is intended for surveying how the *same* source text parses
    /// differently under different `ConnectSpec` versions; the
    /// comparison is not meaningful when applied to unrelated
    /// selections.
    pub fn diff_kinds(&self, other: &Self) -> Vec<DiffKind> {
        let mut out = Vec::new();
        diff_top_level(&self.inner, &other.inner, &mut out);
        out
    }
}

fn diff_top_level(v3: &TopLevelSelection, v4: &TopLevelSelection, out: &mut Vec<DiffKind>) {
    match (v3, v4) {
        (TopLevelSelection::Named(s3), TopLevelSelection::Named(s4)) => {
            diff_subselection(s3, s4, out)
        }
        (TopLevelSelection::Value(l3), TopLevelSelection::Value(l4)) => diff_litexpr(l3, l4, out),
        // The Named→Value flip at top level is the divergent case where the
        // entire selection source is a single primitive token (`null`,
        // `true`, `"foo"`). v0.3 parses it as a NamedSelectionList containing
        // one anonymous Key; v0.4 parses it as a bare LitExpr value.
        (TopLevelSelection::Named(s3), TopLevelSelection::Value(l4)) => {
            if let Some(diff) = classify_top_level_key_to_literal(s3, l4) {
                out.push(diff);
                return;
            }
            out.push(DiffKind::Other {
                v03_variant: "TopLevel::Named",
                v04_variant: "TopLevel::Value",
                source_range: range_to_pair(l4.range()),
            });
        }
        (TopLevelSelection::Value(_), TopLevelSelection::Named(_)) => out.push(DiffKind::Other {
            v03_variant: "TopLevel::Value",
            v04_variant: "TopLevel::Named",
            source_range: None,
        }),
    }
}

/// When the top-level shape flips Named→Value, check if v0.3 had exactly
/// one anonymous NamedSelection wrapping a single primitive Key — that's
/// the top-level form of `KeyFlippedTo…`.
fn classify_top_level_key_to_literal(
    v3: &SubSelection,
    v4: &WithRange<LitExpr>,
) -> Option<DiffKind> {
    let only = match v3.selections.as_slice() {
        [n] => n,
        _ => return None,
    };
    let path = match only.path.as_ref() {
        LitExpr::Path(p) => p,
        _ => return None,
    };
    let key = single_key_of_path(path)?;
    let range = range_to_pair(v4.range());
    // At top level the v0.3 path collapses to a single Key with no tail.
    let followed_by = FollowedBy::Nothing;
    match (key, v4.as_ref()) {
        (Key::Field(name), LitExpr::Null) if name == "null" => {
            Some(DiffKind::KeyFlippedToLiteralNull {
                source_range: range,
                followed_by,
            })
        }
        (Key::Field(name), LitExpr::Bool(value)) if name == "true" || name == "false" => {
            Some(DiffKind::KeyFlippedToLiteralBool {
                value: *value,
                source_range: range,
                followed_by,
            })
        }
        (Key::Field(name), LitExpr::String(_)) => Some(DiffKind::KeyFieldFlippedToLiteralString {
            text: name.clone(),
            source_range: range,
            followed_by,
        }),
        (Key::Quoted(name), LitExpr::String(_)) => {
            Some(DiffKind::KeyQuotedFlippedToLiteralString {
                text: name.clone(),
                source_range: range,
                followed_by,
            })
        }
        _ => None,
    }
}

fn diff_subselection(v3: &SubSelection, v4: &SubSelection, out: &mut Vec<DiffKind>) {
    if v3.selections.len() != v4.selections.len() {
        out.push(DiffKind::Other {
            v03_variant: "SubSelection",
            v04_variant: "SubSelection",
            source_range: range_to_pair(v4.range()),
        });
        return;
    }
    for (n3, n4) in v3.selections.iter().zip(v4.selections.iter()) {
        diff_named_selection(n3, n4, out);
    }
}

fn diff_named_selection(v3: &NamedSelection, v4: &NamedSelection, out: &mut Vec<DiffKind>) {
    diff_litexpr(&v3.path, &v4.path, out);
}

fn diff_litexpr(v3: &WithRange<LitExpr>, v4: &WithRange<LitExpr>, out: &mut Vec<DiffKind>) {
    let v3_inner = v3.as_ref();
    let v4_inner = v4.as_ref();

    // Identical -> nothing to record (recursing into Object/Array/etc handles internal differences).
    if v3_inner == v4_inner {
        return;
    }

    // The breaking class: v0.3 parsed a single-key path that v0.4 sees as a primitive.
    if let LitExpr::Path(path) = v3_inner
        && let Some(key) = single_key_of_path(path)
    {
        match (key, v4_inner) {
            (Key::Field(name), LitExpr::Null) if name == "null" => {
                out.push(DiffKind::KeyFlippedToLiteralNull {
                    source_range: range_to_pair(v4.range()),
                    followed_by: FollowedBy::Nothing,
                });
                return;
            }
            (Key::Field(name), LitExpr::Bool(value)) if name == "true" || name == "false" => {
                out.push(DiffKind::KeyFlippedToLiteralBool {
                    value: *value,
                    source_range: range_to_pair(v4.range()),
                    followed_by: FollowedBy::Nothing,
                });
                return;
            }
            (Key::Field(name), LitExpr::String(s)) => {
                out.push(DiffKind::KeyFieldFlippedToLiteralString {
                    text: name.clone(),
                    source_range: range_to_pair(v4.range()),
                    followed_by: FollowedBy::Nothing,
                });
                let _ = s;
                return;
            }
            (Key::Quoted(name), LitExpr::String(_)) => {
                out.push(DiffKind::KeyQuotedFlippedToLiteralString {
                    text: name.clone(),
                    source_range: range_to_pair(v4.range()),
                    followed_by: FollowedBy::Nothing,
                });
                return;
            }
            _ => {}
        }
    }

    // The cosmetic class: v0.3 parsed `Alias { ... }` as Path-with-only-Selection;
    // v0.4 parses the same source as LitExpr::Object(SubSelection).
    if let (LitExpr::Path(path), LitExpr::Object(obj4)) = (v3_inner, v4_inner)
        && let Some(sub3) = path_starts_with_subselection_only(path)
    {
        out.push(DiffKind::SubSelectionToLitObject {
            source_range: range_to_pair(v4.range()),
        });
        diff_subselection(sub3, obj4, out);
        return;
    }

    // Another cosmetic case: v0.3 used `LitExpr::LegacyObject` (key→value
    // map) for object literals; v0.4 unifies that with `LitExpr::Object`.
    if let (LitExpr::LegacyObject(_), LitExpr::Object(_)) = (v3_inner, v4_inner) {
        out.push(DiffKind::LegacyObjectToLitObject {
            source_range: range_to_pair(v4.range()),
        });
        return;
    }

    // Breaking class extended: v0.3 has a path that *starts with* a
    // primitive-shaped Key followed by more path elements; v0.4 parses the
    // primitive as a literal, with the rest of the path as `LitPath`.
    if let (LitExpr::Path(path), LitExpr::LitPath(root, tail4)) = (v3_inner, v4_inner)
        && let PathList::Key(key, rest3) = path.path.as_ref()
    {
        let range = range_to_pair(v4.range());
        let followed_by = classify_followed_by(tail4);
        let mut emitted = None;
        match (key.as_ref(), root.as_ref()) {
            (Key::Field(name), LitExpr::Null) if name == "null" => {
                emitted = Some(DiffKind::KeyFlippedToLiteralNull {
                    source_range: range,
                    followed_by,
                });
            }
            (Key::Field(name), LitExpr::Bool(value)) if name == "true" || name == "false" => {
                emitted = Some(DiffKind::KeyFlippedToLiteralBool {
                    value: *value,
                    source_range: range,
                    followed_by,
                });
            }
            (Key::Field(name), LitExpr::String(_)) => {
                emitted = Some(DiffKind::KeyFieldFlippedToLiteralString {
                    text: name.clone(),
                    source_range: range,
                    followed_by,
                });
            }
            (Key::Quoted(name), LitExpr::String(_)) => {
                emitted = Some(DiffKind::KeyQuotedFlippedToLiteralString {
                    text: name.clone(),
                    source_range: range,
                    followed_by,
                });
            }
            _ => {}
        }
        if let Some(diff) = emitted {
            out.push(diff);
            diff_pathlist(rest3, tail4, out, v4.range());
            return;
        }
    }

    // Same variant on both sides — recurse into children.
    match (v3_inner, v4_inner) {
        (LitExpr::Object(a), LitExpr::Object(b)) => diff_subselection(a, b, out),
        (LitExpr::Array(a), LitExpr::Array(b)) => {
            if a.len() != b.len() {
                out.push(DiffKind::Other {
                    v03_variant: "LitExpr::Array",
                    v04_variant: "LitExpr::Array",
                    source_range: range_to_pair(v4.range()),
                });
            } else {
                for (x, y) in a.iter().zip(b.iter()) {
                    diff_litexpr(x, y, out);
                }
            }
        }
        (LitExpr::Path(a), LitExpr::Path(b)) => diff_pathselection(a, b, out, v4.range()),
        // Catch-all for shape mismatches we don't classify in detail.
        _ => out.push(DiffKind::Other {
            v03_variant: lit_variant_name(v3_inner),
            v04_variant: lit_variant_name(v4_inner),
            source_range: range_to_pair(v4.range()),
        }),
    }
}

fn diff_pathselection(
    v3: &PathSelection,
    v4: &PathSelection,
    out: &mut Vec<DiffKind>,
    range: OffsetRange,
) {
    diff_pathlist(&v3.path, &v4.path, out, range);
}

fn diff_pathlist(
    v3: &WithRange<PathList>,
    v4: &WithRange<PathList>,
    out: &mut Vec<DiffKind>,
    fallback_range: OffsetRange,
) {
    if v3.as_ref() == v4.as_ref() {
        return;
    }
    match (v3.as_ref(), v4.as_ref()) {
        (PathList::Key(_, tail3), PathList::Key(_, tail4)) => {
            diff_pathlist(tail3, tail4, out, fallback_range);
        }
        (PathList::Var(_, tail3), PathList::Var(_, tail4)) => {
            diff_pathlist(tail3, tail4, out, fallback_range);
        }
        (PathList::Method(_, args3, tail3), PathList::Method(_, args4, tail4)) => {
            match (args3, args4) {
                (Some(a3), Some(a4)) if a3.args.len() == a4.args.len() => {
                    for (x, y) in a3.args.iter().zip(a4.args.iter()) {
                        diff_litexpr(x, y, out);
                    }
                }
                (None, None) => {}
                _ => out.push(DiffKind::Other {
                    v03_variant: "MethodArgs",
                    v04_variant: "MethodArgs",
                    source_range: range_to_pair(fallback_range.clone()),
                }),
            }
            diff_pathlist(tail3, tail4, out, fallback_range);
        }
        (PathList::Expr(e3, tail3), PathList::Expr(e4, tail4)) => {
            diff_litexpr(e3, e4, out);
            diff_pathlist(tail3, tail4, out, fallback_range);
        }
        (PathList::Question(tail3), PathList::Question(tail4)) => {
            diff_pathlist(tail3, tail4, out, fallback_range);
        }
        (PathList::Selection(a), PathList::Selection(b)) => diff_subselection(a, b, out),
        _ => out.push(DiffKind::Other {
            v03_variant: pathlist_variant_name(v3.as_ref()),
            v04_variant: pathlist_variant_name(v4.as_ref()),
            source_range: range_to_pair(v4.range().or(fallback_range)),
        }),
    }
}

fn single_key_of_path(path: &PathSelection) -> Option<&Key> {
    if let PathList::Key(key, tail) = path.path.as_ref()
        && matches!(tail.as_ref(), PathList::Empty)
    {
        return Some(key.as_ref());
    }
    None
}

fn path_starts_with_subselection_only(path: &PathSelection) -> Option<&SubSelection> {
    if let PathList::Selection(sub) = path.path.as_ref() {
        return Some(sub);
    }
    None
}

fn lit_variant_name(l: &LitExpr) -> &'static str {
    match l {
        LitExpr::String(_) => "LitExpr::String",
        LitExpr::Number(_) => "LitExpr::Number",
        LitExpr::Bool(_) => "LitExpr::Bool",
        LitExpr::Null => "LitExpr::Null",
        LitExpr::LegacyObject(_) => "LitExpr::LegacyObject",
        LitExpr::Object(_) => "LitExpr::Object",
        LitExpr::Array(_) => "LitExpr::Array",
        LitExpr::Path(_) => "LitExpr::Path",
        LitExpr::LitPath(_, _) => "LitExpr::LitPath",
        LitExpr::OpChain(_, _) => "LitExpr::OpChain",
    }
}

fn pathlist_variant_name(p: &PathList) -> &'static str {
    match p {
        PathList::Var(_, _) => "PathList::Var",
        PathList::Key(_, _) => "PathList::Key",
        PathList::Expr(_, _) => "PathList::Expr",
        PathList::Method(_, _, _) => "PathList::Method",
        PathList::Question(_) => "PathList::Question",
        PathList::Selection(_) => "PathList::Selection",
        PathList::Empty => "PathList::Empty",
    }
}

fn range_to_pair(r: OffsetRange) -> Option<(usize, usize)> {
    r.map(|range| (range.start, range.end))
}
