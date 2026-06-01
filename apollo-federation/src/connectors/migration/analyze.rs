//! `connect-migrate analyze`: walk a project tree, find
//! `@connect(selection: …)` directives, dual-parse each selection
//! under the schema's linked `connect/v0.n` spec and `ConnectSpec::V0_4`
//! (the migration target), and emit a migration manifest (or JSONL
//! records) sorting each divergent site into rewrites to apply, no-ops,
//! and questions for the developer.
//!
//! The output format is specified in `apollographql/connect-migrate`
//! `SKILL.md` under "Manifest format". This module is the reference
//! implementation; an agent following the skill reads the manifest's
//! `site v2` machine blocks and applies the edits in-session.

use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::Hash;
use std::hash::Hasher;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;

use apollo_parser::Parser as GqlParser;
use apollo_parser::cst;
use apollo_parser::cst::CstNode;
use serde::Serialize;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::connectors::ConnectSpec;
use crate::connectors::JSONSelection;
use crate::connectors::migration::DiffKind;
use crate::connectors::migration::FollowedBy;

/// Per-site record emitted by `analyze`. One entry per breaking
/// `DiffKind` found inside a single `@connect` directive's selection.
/// Cosmetic kinds (`SubSelectionToLitObject`, `LegacyObjectToLitObject`)
/// are filtered out before emission since they have no behavior change.
#[derive(Debug, Serialize, Clone)]
pub struct Site {
    /// 8-hex stable identifier (`DefaultHasher` truncated). Survives
    /// surrounding line shifts in the source file.
    pub id: String,
    /// Project-relative path to the `.graphql` source file.
    pub file: String,
    /// GraphQL schema coordinate (`Type.field`).
    pub coordinate: String,
    /// `DiffKind` variant name in snake_case.
    pub kind: String,
    /// The literal source text of the token at issue. For null/bool
    /// variants this is `"null"`/`"true"`/`"false"`. For string variants
    /// it is the unquoted text.
    pub text: String,
    /// Tail-context classification from the parser.
    pub followed_by: FollowedBy,
    /// Recommended decision per the SKILL.md heuristics.
    pub recommendation: Recommendation,
    /// One-line justification of the recommendation.
    pub reasoning: String,
    /// Line in the source file (1-indexed) where the host `@connect`
    /// directive starts. Authoritative locator — the agent applying
    /// the manifest navigates here to find the directive whose
    /// selection it should rewrite.
    pub line: Option<usize>,
    /// Column in the source file (1-indexed) where the host `@connect`
    /// directive starts. Pair with `line` for navigation.
    pub col: Option<usize>,
    /// Byte offset in the source file where the host `@connect`
    /// directive starts. Same identification semantics as `line`/`col`
    /// but stable under whitespace re-flow.
    pub byte_offset: Option<usize>,
    /// Byte offset range of the divergent token inside `selection`.
    /// Used by the rewrite-block computation to do precise in-place
    /// replacements without text-search heuristics. Not emitted in
    /// markdown output; included in JSON output.
    pub source_range: Option<(usize, usize)>,
    /// The full normalized selection text (`$$` → `$` applied),
    /// included for context display and apply-time lookup.
    pub selection: String,
    /// The `connect/v0.n` spec the host schema links — the "from" side
    /// of this site's upgrade. Read from the schema's
    /// `@link(url: ".../connect/v0.n")`; defaults to `connect/v0.3`
    /// when no connect link is found. The divergence was computed by
    /// parsing the selection at this spec versus `connect/v0.4`.
    pub from_spec: String,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub enum Recommendation {
    #[serde(rename = "keep-v0.3")]
    KeepV03,
    #[serde(rename = "embrace-v0.4")]
    EmbraceV04,
    #[serde(rename = "???")]
    Ambiguous,
}

impl Recommendation {
    fn as_str(self) -> &'static str {
        match self {
            Recommendation::KeepV03 => "keep-v0.3",
            Recommendation::EmbraceV04 => "embrace-v0.4",
            Recommendation::Ambiguous => "???",
        }
    }
}

/// Top-level entry point. Walks the given paths and returns every
/// breaking site found. The order is stable across runs for the same
/// inputs (sites are emitted in file-walk order, then in diff-walk
/// order within each `@connect` directive).
pub fn analyze(paths: &[PathBuf], project_root: &Path) -> AnalyzeReport {
    let mut report = AnalyzeReport::default();
    for p in paths {
        walk(p, project_root, &mut report);
    }
    report
}

/// Summary of an analyze pass. Lets callers distinguish "the analyzer
/// did its job and found nothing actionable" from "the analyzer had
/// nothing to look at" — both produce an empty `sites` vector, but
/// only the former is a trustworthy "safe to upgrade" verdict.
#[derive(Default)]
pub struct AnalyzeReport {
    /// Divergent sites that warrant a developer decision.
    pub sites: Vec<Site>,
    /// `.graphql` files actually visited during the walk.
    pub files_scanned: usize,
    /// Project-relative paths of every `.graphql` file visited, in
    /// walk order. The named scope of the run — lets the manifest
    /// show *which* schemas were considered, not just how many, so a
    /// reader can confirm coverage and catch a missed path.
    pub scanned_paths: Vec<String>,
    /// `connect/v0.n` label of every scanned schema that links the
    /// connect spec, in walk order — one entry per connector schema.
    /// Drives the manifest's `Upgrade` line, which names the source
    /// version(s) being migrated to `connect/v0.4`.
    pub detected_specs: Vec<String>,
    /// Selections that couldn't be diffed because they failed to parse
    /// under the linked spec, under `connect/v0.4`, or both. Surfaced
    /// (non-fatally) so the developer sees what the analysis skipped
    /// rather than having it silently dropped.
    pub parse_notices: Vec<ParseNotice>,
    /// `@connect` directives whose `selection` argument parsed cleanly
    /// under both `connect/v0.3` and `connect/v0.4`. The denominator
    /// for "every selection in your schema parses identically."
    pub directives_analyzed: usize,
}

/// A selection the analysis could not diff because it failed to parse
/// under one or both specs. Non-fatal: the rest of the run proceeds.
#[derive(Debug, Clone)]
pub struct ParseNotice {
    pub file: String,
    pub coordinate: String,
    pub line: Option<usize>,
    pub from_spec: String,
    pub kind: NoticeKind,
}

/// Which side(s) of the parse failed for a [`ParseNotice`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeKind {
    /// Parses under the linked spec but not under `connect/v0.4` — the
    /// selection would break on upgrade and needs a fix first.
    FailsUnderTarget,
    /// Parses under `connect/v0.4` but not under the linked spec — uses
    /// syntax newer than the schema declares (a latent inconsistency).
    FailsUnderSource,
    /// Parses under neither spec — a pre-existing syntax error, out of
    /// scope for migration.
    FailsUnderBoth,
}

/// Machine-readable result kind for the manifest. Surfaced
/// as an HTML comment in the markdown so agents can switch on the
/// outcome without parsing prose.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResultKind {
    /// Walk visited zero `.graphql` files — likely a bad path arg.
    EmptyScan,
    /// Files were scanned but contained no `@connect` directives.
    NothingToMigrate,
    /// Analyzable `@connect` directives are present and every one of
    /// their selections parses identically under v0.3 and v0.4. The
    /// developer can flip the `@link` to `connect/v0.4` without any
    /// source edits.
    SafeToUpgrade,
    /// Divergent selections exist, but every one is mechanically
    /// resolvable with no developer judgement: each site is either a
    /// deterministic `$.` fortification (`keep-v0.3`) or a no-op whose
    /// v0.4 reading is output-identical (`embrace-v0.4`). A clean bill
    /// of health — once the listed rewrites are applied, the upgrade is
    /// safe and there are zero questions for the developer.
    SafeAfterRewrites,
    /// At least one divergent selection is genuinely ambiguous and needs
    /// a developer decision the analyzer cannot make for them.
    NeedsDecisions,
}

impl ResultKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ResultKind::EmptyScan => "empty-scan",
            ResultKind::NothingToMigrate => "nothing-to-migrate",
            ResultKind::SafeToUpgrade => "safe-to-upgrade",
            ResultKind::SafeAfterRewrites => "safe-after-rewrites",
            ResultKind::NeedsDecisions => "needs-decisions",
        }
    }
}

impl AnalyzeReport {
    pub fn result_kind(&self) -> ResultKind {
        if self.files_scanned == 0 {
            ResultKind::EmptyScan
        } else if self.directives_analyzed == 0 {
            ResultKind::NothingToMigrate
        } else if self.sites.is_empty() {
            ResultKind::SafeToUpgrade
        } else if self
            .sites
            .iter()
            .all(|s| s.recommendation != Recommendation::Ambiguous)
        {
            ResultKind::SafeAfterRewrites
        } else {
            ResultKind::NeedsDecisions
        }
    }
}

fn walk(path: &Path, project_root: &Path, report: &mut AnalyzeReport) {
    if path.is_file() {
        if path.extension().and_then(|e| e.to_str()) == Some("graphql") {
            scan_file(path, project_root, report);
        }
        return;
    }
    if path.is_dir() {
        let Ok(entries) = fs::read_dir(path) else {
            return;
        };
        let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        paths.sort();
        for child in paths {
            walk(&child, project_root, report);
        }
    }
}

fn scan_file(path: &Path, project_root: &Path, report: &mut AnalyzeReport) {
    let Ok(sdl) = fs::read_to_string(path) else {
        return;
    };
    report.files_scanned += 1;
    let line_index = LineIndex::new(&sdl);
    let rel = path
        .strip_prefix(project_root)
        .unwrap_or(path)
        .display()
        .to_string();
    report.scanned_paths.push(rel.clone());
    let parser = GqlParser::new(&sdl);
    let cst = parser.parse();
    let doc = cst.document();

    // The "from" side of the upgrade is the spec this schema actually
    // links, not a fixed v0.3 — the selection grammar gates at both
    // v0.2→v0.3 (operator chains) and v0.3→v0.4 (the unification), so
    // the baseline must match the schema's real version. Record it for
    // the manifest's Upgrade line; fall back to v0.3 when no connect
    // link is present (a schema with @connect but no connect @link).
    let detected = detect_connect_spec(&doc);
    if let Some(spec) = detected {
        report
            .detected_specs
            .push(format!("connect/v{}", spec.as_str()));
    }
    let from_spec = detected.unwrap_or(ConnectSpec::V0_3);

    for def in doc.definitions() {
        match def {
            cst::Definition::ObjectTypeDefinition(otd) => {
                let type_name = otd.name().map(|n| n.text().to_string()).unwrap_or_default();
                if let Some(directives) = otd.directives() {
                    scan_directives(&type_name, directives, &rel, from_spec, &line_index, report);
                }
                if let Some(fields) = otd.fields_definition() {
                    scan_fields(&type_name, fields, &rel, from_spec, &line_index, report);
                }
            }
            cst::Definition::ObjectTypeExtension(ote) => {
                let type_name = ote.name().map(|n| n.text().to_string()).unwrap_or_default();
                if let Some(directives) = ote.directives() {
                    scan_directives(&type_name, directives, &rel, from_spec, &line_index, report);
                }
                if let Some(fields) = ote.fields_definition() {
                    scan_fields(&type_name, fields, &rel, from_spec, &line_index, report);
                }
            }
            cst::Definition::InterfaceTypeDefinition(itd) => {
                let type_name = itd.name().map(|n| n.text().to_string()).unwrap_or_default();
                if let Some(directives) = itd.directives() {
                    scan_directives(&type_name, directives, &rel, from_spec, &line_index, report);
                }
                if let Some(fields) = itd.fields_definition() {
                    scan_fields(&type_name, fields, &rel, from_spec, &line_index, report);
                }
            }
            cst::Definition::InterfaceTypeExtension(ite) => {
                let type_name = ite.name().map(|n| n.text().to_string()).unwrap_or_default();
                if let Some(directives) = ite.directives() {
                    scan_directives(&type_name, directives, &rel, from_spec, &line_index, report);
                }
                if let Some(fields) = ite.fields_definition() {
                    scan_fields(&type_name, fields, &rel, from_spec, &line_index, report);
                }
            }
            _ => {}
        }
    }
}

/// Detect the `connect` spec version a schema links by scanning its
/// `schema`/`extend schema` `@link(url: ".../connect/v0.n")` directives.
/// Returns the first connect link found, or `None` if the schema links
/// no connect spec.
fn detect_connect_spec(doc: &cst::Document) -> Option<ConnectSpec> {
    for def in doc.definitions() {
        let directives = match def {
            cst::Definition::SchemaDefinition(s) => s.directives(),
            cst::Definition::SchemaExtension(s) => s.directives(),
            _ => None,
        };
        let Some(directives) = directives else {
            continue;
        };
        for d in directives.directives() {
            if d.name().map(|n| n.text().to_string()).as_deref() != Some("link") {
                continue;
            }
            let Some(args) = d.arguments() else { continue };
            for arg in args.arguments() {
                if arg.name().map(|n| n.text().to_string()).as_deref() != Some("url") {
                    continue;
                }
                if let Some(cst::Value::StringValue(s)) = arg.value()
                    && let Some(spec) =
                        spec_from_link_url(&decode_graphql_string(&s.source_string()))
                {
                    return Some(spec);
                }
            }
        }
    }
    None
}

/// Parse a `connect` spec version out of an `@link` URL like
/// `https://specs.apollo.dev/connect/v0.2`. Returns `None` for URLs
/// that don't name a known connect version.
fn spec_from_link_url(url: &str) -> Option<ConnectSpec> {
    let rest = url.split("/connect/v").nth(1)?;
    let version: String = rest
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let mut parts = version.split('.');
    let major: u32 = parts.next()?.parse().ok()?;
    let minor: u32 = parts.next()?.parse().ok()?;
    match (major, minor) {
        (0, 1) => Some(ConnectSpec::V0_1),
        (0, 2) => Some(ConnectSpec::V0_2),
        (0, 3) => Some(ConnectSpec::V0_3),
        (0, 4) => Some(ConnectSpec::V0_4),
        _ => None,
    }
}

fn scan_directives(
    type_name: &str,
    directives: cst::Directives,
    file: &str,
    from_spec: ConnectSpec,
    line_index: &LineIndex,
    report: &mut AnalyzeReport,
) {
    for d in directives.directives() {
        if d.name().map(|n| n.text().to_string()).as_deref() == Some("connect") {
            handle_connect(
                &d,
                type_name.to_string(),
                file,
                from_spec,
                line_index,
                report,
            );
        }
    }
}

fn scan_fields(
    type_name: &str,
    fields: cst::FieldsDefinition,
    file: &str,
    from_spec: ConnectSpec,
    line_index: &LineIndex,
    report: &mut AnalyzeReport,
) {
    for field in fields.field_definitions() {
        let field_name = field
            .name()
            .map(|n| n.text().to_string())
            .unwrap_or_default();
        let coordinate = format!("{type_name}.{field_name}");
        let Some(directives) = field.directives() else {
            continue;
        };
        for d in directives.directives() {
            if d.name().map(|n| n.text().to_string()).as_deref() == Some("connect") {
                handle_connect(&d, coordinate.clone(), file, from_spec, line_index, report);
            }
        }
    }
}

fn handle_connect(
    d: &cst::Directive,
    coordinate: String,
    file: &str,
    from_spec: ConnectSpec,
    line_index: &LineIndex,
    report: &mut AnalyzeReport,
) {
    let Some(args) = d.arguments() else { return };
    let Some(selection_text) = extract_selection(args) else {
        return;
    };
    // Router config expansion converts `$$` → `$` before parsing
    // selections at runtime, so the at-rest form gets normalized here
    // too. See v04_divergence.rs::compute_verdict for the rationale.
    let normalized = selection_text.replace("$$", "$");
    // The "from" side parses at the schema's actual linked spec, so the
    // diff reflects this schema's real current behavior, not a fixed
    // v0.3 baseline. `connect/v0.4` is always the migration target.
    let from_parse = JSONSelection::parse_with_spec(&normalized, from_spec);
    let v4_parse = JSONSelection::parse_with_spec(&normalized, ConnectSpec::V0_4);
    let (from, v4) = match (from_parse, v4_parse) {
        (Ok(from), Ok(v4)) => (from, v4),
        // One or both sides failed to parse — we can't diff, but we
        // don't silently drop it. Record a non-fatal notice so the
        // developer sees the selection the analysis skipped.
        (from_res, v4_res) => {
            let kind = match (from_res.is_ok(), v4_res.is_ok()) {
                (true, false) => NoticeKind::FailsUnderTarget,
                (false, true) => NoticeKind::FailsUnderSource,
                _ => NoticeKind::FailsUnderBoth,
            };
            let (line_no, _) = line_index.position(usize::from(d.syntax().text_range().start()));
            report.parse_notices.push(ParseNotice {
                file: file.to_string(),
                coordinate,
                line: Some(line_no),
                from_spec: format!("connect/v{}", from_spec.as_str()),
                kind,
            });
            return;
        }
    };
    // Both parses succeeded — the directive contributes to the
    // "safe to upgrade" denominator regardless of whether the two
    // ASTs end up structurally equal.
    report.directives_analyzed += 1;
    if from.structural_eq(&v4) {
        return;
    }
    let diffs = from.diff_kinds(&v4);

    let directive_start = usize::from(d.syntax().text_range().start());
    let (line_no, col_no) = line_index.position(directive_start);
    let byte_offset = directive_start;

    for kind in diffs {
        // Skip cosmetic kinds; analyze only reports actionable sites.
        if matches!(
            kind,
            DiffKind::SubSelectionToLitObject { .. } | DiffKind::LegacyObjectToLitObject { .. }
        ) {
            continue;
        }
        let kind_name = diff_kind_name(&kind);
        let text = diff_kind_text(&kind).to_string();
        let followed_by = diff_kind_followed_by(&kind);
        let source_range = diff_kind_source_range(&kind);
        let (rec, reason) = classify(&kind);
        let id = hash_id(file, &coordinate, kind_name, &text, &normalized);
        report.sites.push(Site {
            id,
            file: file.to_string(),
            coordinate: coordinate.clone(),
            kind: kind_name.to_string(),
            text,
            followed_by,
            recommendation: rec,
            reasoning: reason,
            line: Some(line_no),
            col: Some(col_no),
            byte_offset: Some(byte_offset),
            source_range,
            selection: normalized.clone(),
            from_spec: format!("connect/v{}", from_spec.as_str()),
        });
    }
}

fn diff_kind_source_range(kind: &DiffKind) -> Option<(usize, usize)> {
    match kind {
        DiffKind::KeyFlippedToLiteralNull { source_range, .. } => *source_range,
        DiffKind::KeyFlippedToLiteralBool { source_range, .. } => *source_range,
        DiffKind::KeyFieldFlippedToLiteralString { source_range, .. } => *source_range,
        DiffKind::KeyQuotedFlippedToLiteralString { source_range, .. } => *source_range,
        DiffKind::SubSelectionToLitObject { source_range } => *source_range,
        DiffKind::LegacyObjectToLitObject { source_range } => *source_range,
        DiffKind::Other { source_range, .. } => *source_range,
    }
}

fn extract_selection(args: cst::Arguments) -> Option<String> {
    for arg in args.arguments() {
        let name = arg.name()?.text().to_string();
        if name == "selection"
            && let cst::Value::StringValue(s) = arg.value()?
        {
            return Some(decode_graphql_string(&s.source_string()));
        }
    }
    None
}

fn diff_kind_name(kind: &DiffKind) -> &'static str {
    match kind {
        DiffKind::KeyFlippedToLiteralNull { .. } => "key_flipped_to_literal_null",
        DiffKind::KeyFlippedToLiteralBool { .. } => "key_flipped_to_literal_bool",
        DiffKind::KeyFieldFlippedToLiteralString { .. } => "key_field_flipped_to_literal_string",
        DiffKind::KeyQuotedFlippedToLiteralString { .. } => "key_quoted_flipped_to_literal_string",
        DiffKind::SubSelectionToLitObject { .. } => "sub_selection_to_lit_object",
        DiffKind::LegacyObjectToLitObject { .. } => "legacy_object_to_lit_object",
        DiffKind::Other { .. } => "other",
    }
}

fn diff_kind_text(kind: &DiffKind) -> &str {
    match kind {
        DiffKind::KeyFlippedToLiteralNull { .. } => "null",
        DiffKind::KeyFlippedToLiteralBool { value: true, .. } => "true",
        DiffKind::KeyFlippedToLiteralBool { value: false, .. } => "false",
        DiffKind::KeyFieldFlippedToLiteralString { text, .. } => text,
        DiffKind::KeyQuotedFlippedToLiteralString { text, .. } => text,
        _ => "",
    }
}

fn diff_kind_followed_by(kind: &DiffKind) -> FollowedBy {
    match kind {
        DiffKind::KeyFlippedToLiteralNull { followed_by, .. } => *followed_by,
        DiffKind::KeyFlippedToLiteralBool { followed_by, .. } => *followed_by,
        DiffKind::KeyFieldFlippedToLiteralString { followed_by, .. } => *followed_by,
        DiffKind::KeyQuotedFlippedToLiteralString { followed_by, .. } => *followed_by,
        _ => FollowedBy::Nothing,
    }
}

/// Classify a `DiffKind` into a recommendation bucket using the
/// heuristics from `SKILL.md` Mode A Step A2.
fn classify(kind: &DiffKind) -> (Recommendation, String) {
    match kind {
        DiffKind::KeyQuotedFlippedToLiteralString {
            text, followed_by, ..
        } => {
            if !matches!(followed_by, FollowedBy::Nothing) {
                return (
                    Recommendation::KeepV03,
                    format!(
                        "`\"{text}\"` is followed by a path access — literals don't have fields, so the v0.3 field-reference reading is the intended one."
                    ),
                );
            }
            // A quoted token was unambiguously a quoted field access in
            // v0.3 (`Key::Quoted`) — v0.3 had no string-literal syntax
            // of this shape, so there is nothing to guess. v0.4 silently
            // rereads it as a string literal; fortifying to `$."..."`
            // preserves the original field access. Reading the content
            // as a constant would be the tool *changing* behavior on the
            // developer's behalf, which the migration must not do. If a
            // literal string is genuinely wanted, that is a deliberate
            // post-migration edit.
            (
                Recommendation::KeepV03,
                format!(
                    "`\"{text}\"` was a quoted field access in v0.3, which had no literal-string meaning there; v0.4 silently rereads it as the string literal `\"{text}\"`. Fortifying to `$.\"{text}\"` preserves the original field access."
                ),
            )
        }
        DiffKind::KeyFlippedToLiteralNull { followed_by, .. } => {
            if !matches!(followed_by, FollowedBy::Nothing) {
                return (
                    Recommendation::KeepV03,
                    "`null` is followed by a path access — literals have no fields, so the v0.3 field-reference reading is the intended one.".to_string(),
                );
            }
            (
                Recommendation::EmbraceV04,
                "Bare `null` in value position is almost always intended as a literal null value; v0.3 returned the same thing accidentally via response normalization.".to_string(),
            )
        }
        DiffKind::KeyFlippedToLiteralBool {
            value, followed_by, ..
        } => {
            let lit = if *value { "true" } else { "false" };
            if !matches!(followed_by, FollowedBy::Nothing) {
                return (
                    Recommendation::KeepV03,
                    format!(
                        "`{lit}` is followed by a path access — literals have no fields, so the v0.3 field-reference reading is the intended one."
                    ),
                );
            }
            (
                Recommendation::EmbraceV04,
                format!(
                    "Bare `{lit}` in value position is almost always intended as a literal boolean."
                ),
            )
        }
        DiffKind::KeyFieldFlippedToLiteralString {
            text, followed_by, ..
        } => {
            if !matches!(followed_by, FollowedBy::Nothing) {
                return (
                    Recommendation::KeepV03,
                    format!("`{text}` is followed by a path access — literals have no fields."),
                );
            }
            // Same principle as the quoted case: a bare identifier was a
            // field reference in v0.3 (`Key::Field`), with no bare
            // string-literal syntax available. v0.4 rereads it as the
            // string literal `"{text}"`; fortifying to `$.{text}`
            // preserves the original field access.
            (
                Recommendation::KeepV03,
                format!(
                    "`{text}` was a bare field reference in v0.3; v0.4 rereads it as the string literal `\"{text}\"`. Fortifying to `$.{text}` preserves the original field access."
                ),
            )
        }
        DiffKind::SubSelectionToLitObject { .. }
        | DiffKind::LegacyObjectToLitObject { .. }
        | DiffKind::Other { .. } => (
            Recommendation::Ambiguous,
            "Unclassified diff — please review manually.".to_string(),
        ),
    }
}

fn hash_id(file: &str, coordinate: &str, kind: &str, text: &str, selection: &str) -> String {
    let mut hasher = DefaultHasher::new();
    file.hash(&mut hasher);
    coordinate.hash(&mut hasher);
    kind.hash(&mut hasher);
    text.hash(&mut hasher);
    selection.hash(&mut hasher);
    let h = hasher.finish();
    // First 4 bytes (8 hex chars). Collision is uninteresting for
    // human-scale recommendation files; apply double-checks against
    // file/coordinate/text on lookup anyway.
    format!("{:08x}", (h as u32))
}

/// Return the `$.`-fortified replacement text for a divergent token,
/// or `None` if the token's kind isn't one we know how to fortify.
fn fortification_for(site: &Site) -> Option<String> {
    Some(match site.kind.as_str() {
        "key_flipped_to_literal_null" => "$.null".to_string(),
        "key_flipped_to_literal_bool" => format!("$.{}", site.text),
        "key_field_flipped_to_literal_string" => format!("$.{}", site.text),
        "key_quoted_flipped_to_literal_string" => format!("$.\"{}\"", site.text),
        _ => return None,
    })
}

/// Emit the `Upgrade:` line naming the source connect version(s) found
/// across the scanned schemas and the `connect/v0.4` target. Reads
/// `report.detected_specs` (one label per connector schema), so a mixed
/// corpus reads e.g. "connect/v0.2 (7 schemas) · connect/v0.3 (5
/// schemas) → connect/v0.4".
fn write_upgrade<W: Write>(out: &mut W, report: &AnalyzeReport) -> std::io::Result<()> {
    let target = format!("connect/v{}", ConnectSpec::V0_4.as_str());
    if report.detected_specs.is_empty() {
        writeln!(
            out,
            "**Upgrade:** → `{target}` (no source connect version detected; assumed `connect/v0.3`)"
        )?;
        return Ok(());
    }
    let mut counts: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for s in &report.detected_specs {
        *counts.entry(s.as_str()).or_default() += 1;
    }
    let froms = counts
        .iter()
        .map(|(v, n)| format!("`{v}` ({n} schema{})", if *n == 1 { "" } else { "s" }))
        .collect::<Vec<_>>()
        .join(" · ");
    writeln!(out, "**Upgrade:** {froms} → `{target}`")?;
    Ok(())
}

/// Emit the non-fatal "heads up" section for selections that failed to
/// parse under one or both specs, so they're visible rather than
/// silently dropped. Writes nothing when there are no notices.
fn write_parse_notices<W: Write>(out: &mut W, report: &AnalyzeReport) -> std::io::Result<()> {
    if report.parse_notices.is_empty() {
        return Ok(());
    }
    writeln!(out)?;
    writeln!(
        out,
        "## Heads up — selections not analyzed ({})",
        report.parse_notices.len()
    )?;
    writeln!(out)?;
    writeln!(
        out,
        "These `@connect` selections failed to parse under one or both specs, so they aren't in the buckets above. Non-fatal — the rest of the analysis stands — but review each:"
    )?;
    writeln!(out)?;
    for n in &report.parse_notices {
        let loc = match n.line {
            Some(l) => format!("{}:{}", n.file, l),
            None => n.file.clone(),
        };
        let msg = match n.kind {
            NoticeKind::FailsUnderTarget => format!(
                "parses under `{}` but **not** under `connect/v0.4` — this selection would break on upgrade; fix it before migrating",
                n.from_spec
            ),
            NoticeKind::FailsUnderSource => format!(
                "parses under `connect/v0.4` but **not** under the linked `{}` — it uses syntax newer than the schema declares",
                n.from_spec
            ),
            NoticeKind::FailsUnderBoth => {
                "parses under neither the linked spec nor `connect/v0.4` — a pre-existing syntax error, out of scope for migration".to_string()
            }
        };
        writeln!(out, "- `{coord}` (`{loc}`): {msg}", coord = n.coordinate)?;
    }
    Ok(())
}

/// Emit the named scope of the run — project root, the schema files
/// considered, and the directive count — directly under the title, so
/// a reader can confirm coverage (the right schemas were included, none
/// missed) rather than infer it from a bare count.
fn write_scope<W: Write>(
    out: &mut W,
    report: &AnalyzeReport,
    project_root: &Path,
) -> std::io::Result<()> {
    writeln!(
        out,
        "**Scope:** project root `{root}` · {files} · {directives} analyzed.",
        root = project_root.display(),
        files = file_count_phrase(report.files_scanned),
        directives = directive_count_phrase(report.directives_analyzed),
    )?;
    if !report.scanned_paths.is_empty() {
        writeln!(out)?;
        let listed = report
            .scanned_paths
            .iter()
            .map(|p| format!("`{p}`"))
            .collect::<Vec<_>>()
            .join(" · ");
        writeln!(out, "Schemas considered: {listed}")?;
    }
    Ok(())
}

/// Emit the migration manifest per the SKILL.md v2 format.
pub fn write_markdown<W: Write>(
    out: &mut W,
    report: &AnalyzeReport,
    project_root: &Path,
    generator_version: &str,
) -> std::io::Result<()> {
    let generated_at = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "unknown".to_string());
    let kind = report.result_kind();

    // Partition every divergent site by the decision it implies. These
    // three buckets are the spine of the manifest: deterministic
    // rewrites the agent applies, no-ops it can ignore, and genuine
    // questions it must put to the developer.
    let auto_fixes: Vec<&Site> = report
        .sites
        .iter()
        .filter(|s| s.recommendation == Recommendation::KeepV03)
        .collect();
    let no_ops: Vec<&Site> = report
        .sites
        .iter()
        .filter(|s| s.recommendation == Recommendation::EmbraceV04)
        .collect();
    let questions: Vec<&Site> = report
        .sites
        .iter()
        .filter(|s| s.recommendation == Recommendation::Ambiguous)
        .collect();

    writeln!(out, "<!-- connect-migrate manifest v2 -->")?;
    writeln!(out, "<!-- result: {} -->", kind.as_str())?;
    writeln!(
        out,
        "<!-- generator: connect-migrate {generator_version} -->"
    )?;
    writeln!(out, "<!-- generated-at: {generated_at} -->")?;
    writeln!(out, "<!-- project-root: {} -->", project_root.display())?;
    writeln!(out, "<!-- files-scanned: {} -->", report.files_scanned)?;
    writeln!(
        out,
        "<!-- directives-analyzed: {} -->",
        report.directives_analyzed
    )?;
    writeln!(out, "<!-- divergent-sites: {} -->", report.sites.len())?;
    writeln!(out, "<!-- auto-fixes: {} -->", auto_fixes.len())?;
    writeln!(out, "<!-- no-ops: {} -->", no_ops.len())?;
    writeln!(out, "<!-- questions: {} -->", questions.len())?;
    {
        let mut froms: Vec<&str> = report.detected_specs.iter().map(String::as_str).collect();
        froms.sort_unstable();
        froms.dedup();
        writeln!(
            out,
            "<!-- upgrade: {} -> connect/v{} -->",
            froms.join(","),
            ConnectSpec::V0_4.as_str(),
        )?;
    }
    writeln!(
        out,
        "<!-- parse-notices: {} -->",
        report.parse_notices.len()
    )?;
    writeln!(out)?;

    match kind {
        ResultKind::EmptyScan => {
            writeln!(out, "# connect-migrate report — no schema files found")?;
            writeln!(out)?;
            writeln!(
                out,
                "Scanned 0 `.graphql` file(s). The path you passed contains no GraphQL schemas the analyzer could see. Confirm the directory or file argument and re-run."
            )?;
            return Ok(());
        }
        ResultKind::NothingToMigrate => {
            writeln!(
                out,
                "# connect-migrate report — no `@connect` directives present"
            )?;
            writeln!(out)?;
            write_scope(out, report, project_root)?;
            // No connect directives → no upgrade to name.
            write_parse_notices(out, report)?;
            writeln!(out)?;
            if report.parse_notices.is_empty() {
                writeln!(
                    out,
                    "None of the schemas above declare a `@connect` directive, so there is nothing for this tool to migrate. If your connector schemas live elsewhere, re-run against that path; otherwise the project does not use Apollo Connectors and the `connect/v0.4` upgrade does not apply."
                )?;
            } else {
                writeln!(
                    out,
                    "No `@connect` selection parsed cleanly under both the linked spec and `connect/v0.4`, so there is nothing to migrate automatically — see the heads-up above and resolve those selections first."
                )?;
            }
            return Ok(());
        }
        ResultKind::SafeToUpgrade => {
            writeln!(out, "# connect-migrate manifest — safe to upgrade")?;
            writeln!(out)?;
            write_upgrade(out, report)?;
            writeln!(out)?;
            write_scope(out, report, project_root)?;
            writeln!(out)?;
            writeln!(
                out,
                "Every `@connect(selection: …)` across the schemas above parses identically under `connect/v0.3` and `connect/v0.4` — zero divergent selections. This is a trustworthy verdict from the analyzer, not the absence of one: the upgrade is safe with no source changes."
            )?;
            writeln!(out)?;
            writeln!(
                out,
                "Update your schema's `@link` to `connect/v0.4` whenever you're ready:"
            )?;
            writeln!(out)?;
            writeln!(out, "```graphql")?;
            writeln!(out, "extend schema")?;
            writeln!(
                out,
                "  @link(url: \"https://specs.apollo.dev/connect/v0.4\", import: [\"@connect\", \"@source\"])"
            )?;
            writeln!(out, "```")?;
            write_parse_notices(out, report)?;
            return Ok(());
        }
        ResultKind::SafeAfterRewrites | ResultKind::NeedsDecisions => {}
    }

    // Title + one-line verdict.
    match kind {
        ResultKind::SafeAfterRewrites => {
            writeln!(out, "# connect-migrate manifest — safe after rewrites")?;
            writeln!(out)?;
            write_upgrade(out, report)?;
            writeln!(out)?;
            write_scope(out, report, project_root)?;
            writeln!(out)?;
            writeln!(
                out,
                "Found {sites} divergent token(s) — **every one is mechanically resolvable, with no developer decisions required.** Apply the rewrites below and the `connect/v0.4` upgrade is safe.",
                sites = report.sites.len(),
            )?;
        }
        ResultKind::NeedsDecisions => {
            writeln!(out, "# connect-migrate manifest — decisions needed")?;
            writeln!(out)?;
            write_upgrade(out, report)?;
            writeln!(out)?;
            write_scope(out, report, project_root)?;
            writeln!(out)?;
            writeln!(
                out,
                "Found {sites} divergent token(s): {fixes} to rewrite, {noops} no-op(s), and **{qs} question(s)** that need a developer decision the analyzer cannot make. Apply the rewrites, then work the questions below.",
                sites = report.sites.len(),
                fixes = auto_fixes.len(),
                noops = no_ops.len(),
                qs = questions.len(),
            )?;
        }
        _ => unreachable!("trivial result kinds returned above"),
    }
    write_parse_notices(out, report)?;
    writeln!(out)?;

    // --- Rewrites to apply ---------------------------------------------
    writeln!(out, "## Rewrites to apply ({})", auto_fixes.len())?;
    writeln!(out)?;
    if auto_fixes.is_empty() {
        writeln!(out, "None — no source edits are required.")?;
    } else {
        writeln!(
            out,
            "Deterministic `$.` fortifications. In v0.3 each of these was a field access; v0.4 silently rereads it as a literal, so fortifying preserves the original behavior. Apply each edit at the location in its machine block (`rewrite_to` is the exact replacement text for the `text` token)."
        )?;
        writeln!(out)?;
        let fix_groups = group_indistinguishable_sites(&auto_fixes);
        for group in &fix_groups {
            write_site_block(out, group)?;
        }
        writeln!(out)?;
        for group in &fix_groups {
            let site = group.site;
            let mult = occurrence_suffix(group.count);
            let to = fortification_for(site)
                .map(|s| format!("`{s}`"))
                .unwrap_or_else(|| "—".to_string());
            writeln!(
                out,
                "- `{text}` → {to}{mult} — `{loc}` (`{coord}`)",
                text = site.text,
                loc = site_location(site),
                coord = site.coordinate,
            )?;
        }
        writeln!(out)?;
        writeln!(
            out,
            "This list is the source of truth for what gets applied. To **skip** a rewrite, delete its bullet and its `site v2` block above; to **change** a replacement, edit that block's `rewrite_to`. The agent applies exactly the blocks that remain, using each `rewrite_to` verbatim — nothing more."
        )?;
    }
    writeln!(out)?;

    // --- No action needed ----------------------------------------------
    writeln!(out, "## No action needed ({})", no_ops.len())?;
    writeln!(out)?;
    if no_ops.is_empty() {
        writeln!(out, "None.")?;
    } else {
        let mut by_token: std::collections::BTreeMap<&str, usize> =
            std::collections::BTreeMap::new();
        let mut selections: std::collections::BTreeSet<(&str, &str)> =
            std::collections::BTreeSet::new();
        for s in &no_ops {
            *by_token.entry(s.text.as_str()).or_default() += 1;
            selections.insert((s.file.as_str(), s.coordinate.as_str()));
        }
        writeln!(
            out,
            "{n} token(s) across {sel} selection(s) parse differently under v0.4 but evaluate to the same value — a bare `null`/`true`/`false` that v0.3 resolved via response normalization. No edits required.",
            n = no_ops.len(),
            sel = selections.len(),
        )?;
        writeln!(out)?;
        for (token, count) in &by_token {
            writeln!(out, "- `{token}` ×{count}")?;
        }
    }
    writeln!(out)?;

    // --- Questions for the developer -----------------------------------
    writeln!(out, "## Questions for the developer ({})", questions.len())?;
    writeln!(out)?;
    if questions.is_empty() {
        writeln!(
            out,
            "None — every divergence resolved mechanically. Clean bill of health."
        )?;
    } else {
        writeln!(
            out,
            "Each item is a genuine fork the analyzer cannot resolve. Put it to the developer; do not guess. Sites that share one decision are grouped — a single answer applies to the whole group."
        )?;
        writeln!(out)?;
        let q_groups = group_indistinguishable_sites(&questions);
        for (i, group) in q_groups.iter().enumerate() {
            let site = group.site;
            writeln!(out, "### Question {} of {}", i + 1, q_groups.len())?;
            writeln!(out)?;
            write_site_block(out, group)?;
            writeln!(out)?;
            writeln!(
                out,
                "`{coord}` in `{loc}`{mult}: {reasoning}",
                coord = site.coordinate,
                loc = site_location(site),
                mult = occurrence_suffix(group.count),
                reasoning = site.reasoning,
            )?;
            writeln!(out)?;
            let one = [site];
            let windows = compute_windows(&site.selection, &one, WINDOW_CONTEXT_LINES);
            writeln!(out, "```graphql")?;
            write_windowed(out, &site.selection, &windows)?;
            writeln!(out, "```")?;
            writeln!(out)?;
        }
    }

    // --- After applying: switch to connect/v0.4 ------------------------
    writeln!(out)?;
    writeln!(out, "## After applying — switch to `connect/v0.4`")?;
    writeln!(out)?;
    let outstanding = if questions.is_empty() {
        "Once the rewrites above are applied"
    } else {
        "Once the rewrites are applied and the questions resolved"
    };
    writeln!(
        out,
        "{outstanding}, bump each migrated schema's connect `@link` to `connect/v0.4` — that's the version this manifest's diff targeted, and the one the rewrites make safe:"
    )?;
    writeln!(out)?;
    writeln!(out, "```graphql")?;
    writeln!(
        out,
        "@link(url: \"https://specs.apollo.dev/connect/v0.4\", import: [\"@connect\", \"@source\"])"
    )?;
    writeln!(out, "```")?;
    writeln!(out)?;
    writeln!(
        out,
        "Update the existing `@link(url: \".../connect/v0.n\")` in place — keep each schema's other links (federation, etc.) untouched. Re-run `connect-migrate analyze` afterward to confirm a clean `safe-to-upgrade` verdict."
    )?;
    Ok(())
}

/// Emit the machine-readable identity block for a site (or site group).
/// Agents parse this to locate and apply the edit without re-deriving
/// anything; `rewrite_to` carries the exact replacement for `text`.
fn write_site_block<W: Write>(out: &mut W, group: &SiteGroup<'_>) -> std::io::Result<()> {
    let site = group.site;
    writeln!(out, "<!-- connect-migrate site v2")?;
    writeln!(out, "  id: {}", site.id)?;
    writeln!(out, "  file: {}", site.file)?;
    if let Some(line) = site.line {
        writeln!(out, "  line: {line}")?;
    }
    if let Some(col) = site.col {
        writeln!(out, "  col: {col}")?;
    }
    if let Some(byte_offset) = site.byte_offset {
        writeln!(out, "  byte_offset: {byte_offset}")?;
    }
    writeln!(out, "  coordinate: {}", site.coordinate)?;
    writeln!(out, "  from: {}", site.from_spec)?;
    writeln!(out, "  kind: {}", site.kind)?;
    if !site.text.is_empty() {
        writeln!(out, "  text: {}", quote_for_comment(&site.text))?;
    }
    if let Some((start, end)) = site.source_range {
        writeln!(out, "  source_range: {start}..{end}")?;
    }
    writeln!(out, "  followed_by: {}", followed_by_name(site.followed_by))?;
    writeln!(out, "  recommendation: {}", site.recommendation.as_str())?;
    if let Some(rewrite) = fortification_for(site) {
        writeln!(out, "  rewrite_to: {}", quote_for_comment(&rewrite))?;
    }
    if group.count > 1 {
        writeln!(out, "  occurrences: {}", group.count)?;
    }
    writeln!(out, "-->")?;
    Ok(())
}

/// `file:line` locator for a site, falling back to bare file when the
/// line is unknown. The directive's start position; the agent finds the
/// exact token via `text` + `source_range`.
fn site_location(site: &Site) -> String {
    match site.line {
        Some(line) => format!("{}:{}", site.file, line),
        None => site.file.clone(),
    }
}

/// `" (×N)"` suffix for a grouped site, or empty for a singleton.
fn occurrence_suffix(count: usize) -> String {
    if count > 1 {
        format!(" (×{count})")
    } else {
        String::new()
    }
}

/// Number of selection lines to show on either side of each divergent
/// token. Larger numbers give more context at the cost of file size;
/// 5 keeps the windowed view scannable while leaving enough room to
/// see the immediate `field: value` siblings.
const WINDOW_CONTEXT_LINES: usize = 5;

/// A group of section sites that the analyzer cannot distinguish from
/// one another at the display level (same `kind`, `text`, `reasoning`,
/// `recommendation`). The site identity hash collapses such groups
/// because position-within-selection isn't part of the hash inputs;
/// rather than print N near-identical identity comments and bullets,
/// emit one entry per group with `occurrences: N` / `(×N)`.
struct SiteGroup<'a> {
    site: &'a Site,
    count: usize,
}

fn group_indistinguishable_sites<'a>(sites: &'a [&'a Site]) -> Vec<SiteGroup<'a>> {
    let mut out: Vec<SiteGroup<'a>> = Vec::new();
    for site in sites {
        let match_idx = out.iter().position(|g| {
            let s = g.site;
            s.id == site.id
                && s.kind == site.kind
                && s.text == site.text
                && s.reasoning == site.reasoning
                && s.recommendation == site.recommendation
                && s.followed_by == site.followed_by
        });
        match match_idx {
            Some(i) => {
                if let Some(group) = out.get_mut(i) {
                    group.count += 1;
                }
            }
            None => out.push(SiteGroup { site, count: 1 }),
        }
    }
    out
}

/// Compute line-range windows over `selection` covering every site's
/// `source_range`, with `context` lines on either side. Adjacent or
/// overlapping windows merge into one. Returns 0-based (inclusive,
/// inclusive) line index pairs in ascending order.
fn compute_windows(selection: &str, sites: &[&Site], context: usize) -> Vec<(usize, usize)> {
    let total = selection.lines().count();
    if total == 0 {
        return Vec::new();
    }
    let last = total.saturating_sub(1);
    let mut anchors: Vec<usize> = Vec::new();
    for site in sites {
        let Some((start, _)) = site.source_range else {
            continue;
        };
        let clamped = start.min(selection.len());
        let line = selection[..clamped].matches('\n').count();
        anchors.push(line);
    }
    anchors.sort();
    let mut windows: Vec<(usize, usize)> = Vec::new();
    for a in anchors {
        let lo = a.saturating_sub(context);
        let hi = a.saturating_add(context).min(last);
        match windows.last_mut() {
            // Merge if the new window overlaps or is adjacent to the
            // previous one (separator line would be empty otherwise).
            Some(prev) if lo <= prev.1.saturating_add(1) => {
                prev.1 = prev.1.max(hi);
            }
            _ => windows.push((lo, hi)),
        }
    }
    windows
}

/// Emit only the lines in `body` covered by `windows`, separating
/// non-adjacent windows with an elision comment so the reader can see
/// where the gaps are. Falls back to writing the full body when
/// `windows` is empty (defensive — every section should have at least
/// one site, but the analyzer can't assume that downstream).
fn write_windowed<W: Write>(
    out: &mut W,
    body: &str,
    windows: &[(usize, usize)],
) -> std::io::Result<()> {
    if windows.is_empty() {
        return write_block_lines(out, body);
    }
    let lines: Vec<&str> = body.lines().collect();
    let total = lines.len();
    if total == 0 {
        return Ok(());
    }
    let last = total.saturating_sub(1);
    let mut prev_hi: Option<usize> = None;
    for &(lo, hi) in windows {
        let hi = hi.min(last);
        let lo = lo.min(hi);
        match prev_hi {
            None if lo > 0 => writeln!(out, "  # …")?,
            Some(p) if lo > p + 1 => writeln!(out, "  # …")?,
            _ => {}
        }
        if let Some(slice) = lines.get(lo..=hi) {
            for line in slice {
                writeln!(out, "{line}")?;
            }
        }
        prev_hi = Some(hi);
    }
    if let Some(p) = prev_hi
        && p + 1 < total
    {
        writeln!(out, "  # …")?;
    }
    Ok(())
}

fn file_count_phrase(n: usize) -> String {
    match n {
        1 => "1 `.graphql` file".to_string(),
        _ => format!("{n} `.graphql` files"),
    }
}

fn directive_count_phrase(n: usize) -> String {
    match n {
        1 => "1 `@connect` directive".to_string(),
        _ => format!("{n} `@connect` directives"),
    }
}

fn write_block_lines<W: Write>(out: &mut W, body: &str) -> std::io::Result<()> {
    for line in body.lines() {
        writeln!(out, "{line}")?;
    }
    Ok(())
}

fn followed_by_name(f: FollowedBy) -> &'static str {
    match f {
        FollowedBy::Nothing => "nothing",
        FollowedBy::KeyAccess => "key_access",
        FollowedBy::Method => "method",
        FollowedBy::SubSelection => "sub_selection",
        FollowedBy::Question => "question",
        FollowedBy::Expr => "expr",
    }
}

/// Escape a string for safe inclusion inside an HTML comment field
/// (no `--` sequences, no newlines).
/// Render a string as a double-quoted, JSON-style escaped value safe to
/// embed inside an HTML comment. Inner quotes and backslashes are
/// escaped so values like `$."USD"` round-trip unambiguously; `--` is
/// broken up so the value can never close the enclosing `-->`.
fn quote_for_comment(s: &str) -> String {
    let cleaned = s
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace("--", "-\\-");
    format!("\"{cleaned}\"")
}

/// Emit one JSONL record per site to `out`, one record per line.
pub fn write_jsonl<W: Write>(out: &mut W, sites: &[Site]) -> std::io::Result<()> {
    for site in sites {
        let line = serde_json::to_string(site)
            .unwrap_or_else(|_| String::from("{\"error\":\"serialize\"}"));
        writeln!(out, "{line}")?;
    }
    Ok(())
}

// ---- GraphQL string decode (mirrored from corpus extractor) ----

fn decode_graphql_string(source: &str) -> String {
    let s = source;
    if let Some(inner) = strip_pair(s, "\"\"\"") {
        return block_string_value(inner);
    }
    if let Some(inner) = strip_pair(s, "'''") {
        return block_string_value(inner);
    }
    if let Some(inner) = strip_pair(s, "\"") {
        return single_line_value(inner);
    }
    if let Some(inner) = strip_pair(s, "'") {
        return single_line_value(inner);
    }
    s.to_string()
}

fn strip_pair<'a>(s: &'a str, delim: &str) -> Option<&'a str> {
    if s.starts_with(delim) && s.ends_with(delim) && s.len() >= 2 * delim.len() {
        Some(&s[delim.len()..s.len() - delim.len()])
    } else {
        None
    }
}

fn block_string_value(raw: &str) -> String {
    let lines: Vec<&str> = raw.split('\n').collect();
    let mut common_indent: Option<usize> = None;
    for (i, line) in lines.iter().enumerate() {
        if i == 0 {
            continue;
        }
        let indent = line
            .bytes()
            .take_while(|b| *b == b' ' || *b == b'\t')
            .count();
        if indent < line.len() {
            common_indent = Some(match common_indent {
                Some(c) => c.min(indent),
                None => indent,
            });
        }
    }
    let common = common_indent.unwrap_or(0);
    let mut stripped: Vec<String> = Vec::with_capacity(lines.len());
    for (i, line) in lines.iter().enumerate() {
        if i == 0 {
            stripped.push((*line).to_string());
        } else if line.len() >= common {
            stripped.push(line[common..].to_string());
        } else {
            stripped.push(String::new());
        }
    }
    while stripped
        .first()
        .map(|l| l.trim().is_empty())
        .unwrap_or(false)
    {
        stripped.remove(0);
    }
    while stripped
        .last()
        .map(|l| l.trim().is_empty())
        .unwrap_or(false)
    {
        stripped.pop();
    }
    let mut out = stripped.join("\n");
    out = out.replace("\\\"\"\"", "\"\"\"");
    out
}

fn single_line_value(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('"') => out.push('"'),
            Some('\'') => out.push('\''),
            Some('\\') => out.push('\\'),
            Some('/') => out.push('/'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some('b') => out.push('\u{0008}'),
            Some('f') => out.push('\u{000C}'),
            Some('u') => {
                let mut hex = String::new();
                if chars.peek() == Some(&'{') {
                    chars.next();
                    while let Some(&p) = chars.peek() {
                        if p == '}' {
                            chars.next();
                            break;
                        }
                        hex.push(p);
                        chars.next();
                    }
                } else {
                    for _ in 0..4 {
                        if let Some(p) = chars.next() {
                            hex.push(p);
                        }
                    }
                }
                if let Ok(n) = u32::from_str_radix(&hex, 16)
                    && let Some(ch) = char::from_u32(n)
                {
                    out.push(ch);
                }
            }
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

// ---- Line/column index for byte offsets into a source string ----

struct LineIndex {
    line_starts: Vec<usize>,
}

impl LineIndex {
    fn new(source: &str) -> Self {
        let mut line_starts = vec![0];
        for (i, b) in source.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push(i + 1);
            }
        }
        Self { line_starts }
    }

    /// Convert a byte offset into a (1-indexed line, 1-indexed col).
    fn position(&self, offset: usize) -> (usize, usize) {
        let line_idx = match self.line_starts.binary_search(&offset) {
            Ok(i) => i,
            Err(i) => i.saturating_sub(1),
        };
        let line_start = self.line_starts.get(line_idx).copied().unwrap_or(0);
        (line_idx + 1, offset.saturating_sub(line_start) + 1)
    }
}
