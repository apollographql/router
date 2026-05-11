//! Survey how `JSONSelection` ASTs differ between `ConnectSpec::V0_3` and
//! `ConnectSpec::V0_4`. Reads JSONL records (one per `@connect` directive)
//! on stdin or from a file argument; writes JSONL on stdout with each
//! input record augmented by parse status, divergence verdict, and an
//! itemized list of `DiffKind`s for divergent parses.
//!
//! Input record shape (from the connectors-corpus extractor):
//!     { "file": "...", "subgraph": "...", "coordinate": "Type.field",
//!       "selection": "...", "has_selection_arg": true, ... }
//!
//! Output record shape: input record plus:
//!     { "v03_parse": "ok" | "err:<msg>",
//!       "v04_parse": "ok" | "err:<msg>",
//!       "divergence": "none" | "ast_differs" | "v04_only_accepts"
//!                   | "v03_only_accepts" | "both_reject" | "no_selection",
//!       "diff_kinds": [ { "kind": "...", ... }, ... ] }

use std::collections::BTreeMap;
use std::env;
use std::fs::File;
use std::io::{self, BufRead, BufReader, BufWriter, Write};

use apollo_federation::connectors::{ConnectSpec, DiffKind, JSONSelection};
use serde_json::Value;

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let reader: Box<dyn BufRead> = match args.as_slice() {
        [] => Box::new(BufReader::new(io::stdin())),
        [path] => match File::open(path) {
            Ok(f) => Box::new(BufReader::new(f)),
            Err(e) => {
                eprintln!("open {path}: {e}");
                std::process::exit(2);
            }
        },
        _ => {
            eprintln!("usage: v04_divergence [path-to-jsonl]");
            std::process::exit(2);
        }
    };

    let stdout = io::stdout();
    let mut writer = BufWriter::new(stdout.lock());

    let mut counters = Counters::default();

    for (line_no, line) in reader.lines().enumerate() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                eprintln!("read error on line {}: {e}", line_no + 1);
                continue;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let mut record: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("json parse error on line {}: {e}", line_no + 1);
                continue;
            }
        };

        let verdict = compute_verdict(&record);
        counters.tally(&verdict);

        let obj = record.as_object_mut().expect("input record must be an object");
        obj.insert("v03_parse".to_string(), Value::String(verdict.v03_parse.clone()));
        obj.insert("v04_parse".to_string(), Value::String(verdict.v04_parse.clone()));
        obj.insert(
            "divergence".to_string(),
            Value::String(verdict.divergence.as_str().to_string()),
        );
        if !verdict.diff_kinds.is_empty() {
            obj.insert(
                "diff_kinds".to_string(),
                serde_json::to_value(&verdict.diff_kinds).unwrap_or(Value::Null),
            );
        }

        if let Err(e) = writeln!(writer, "{}", record) {
            eprintln!("write error: {e}");
            std::process::exit(1);
        }
    }

    let _ = writer.flush();
    counters.report();
}

#[derive(Debug)]
struct Verdict {
    v03_parse: String,
    v04_parse: String,
    divergence: Divergence,
    diff_kinds: Vec<DiffKind>,
}

#[derive(Debug, Clone, Copy)]
enum Divergence {
    None,
    AstDiffers,
    V04OnlyAccepts,
    V03OnlyAccepts,
    BothReject,
    NoSelection,
}

impl Divergence {
    fn as_str(self) -> &'static str {
        match self {
            Divergence::None => "none",
            Divergence::AstDiffers => "ast_differs",
            Divergence::V04OnlyAccepts => "v04_only_accepts",
            Divergence::V03OnlyAccepts => "v03_only_accepts",
            Divergence::BothReject => "both_reject",
            Divergence::NoSelection => "no_selection",
        }
    }
}

fn compute_verdict(record: &Value) -> Verdict {
    let selection = match record.get("selection").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => {
            return Verdict {
                v03_parse: "n/a".into(),
                v04_parse: "n/a".into(),
                divergence: Divergence::NoSelection,
                diff_kinds: Vec::new(),
            };
        }
    };

    // Router config expansion converts `$$` -> `$` before parsing selections,
    // so the at-rest YAML form (`$$args.x`) becomes (`$args.x`) at runtime.
    let normalized = selection.replace("$$", "$");
    let v3 = JSONSelection::parse_with_spec(&normalized, ConnectSpec::V0_3);
    let v4 = JSONSelection::parse_with_spec(&normalized, ConnectSpec::V0_4);

    let v03_parse = match &v3 {
        Ok(_) => "ok".to_string(),
        Err(e) => format!("err:{}", short_err(&e.message)),
    };
    let v04_parse = match &v4 {
        Ok(_) => "ok".to_string(),
        Err(e) => format!("err:{}", short_err(&e.message)),
    };

    let (divergence, diff_kinds) = match (&v3, &v4) {
        (Ok(a), Ok(b)) => {
            if a.structural_eq(b) {
                (Divergence::None, Vec::new())
            } else {
                (Divergence::AstDiffers, a.diff_kinds(b))
            }
        }
        (Err(_), Ok(_)) => (Divergence::V04OnlyAccepts, Vec::new()),
        (Ok(_), Err(_)) => (Divergence::V03OnlyAccepts, Vec::new()),
        (Err(_), Err(_)) => (Divergence::BothReject, Vec::new()),
    };

    Verdict { v03_parse, v04_parse, divergence, diff_kinds }
}

fn short_err(msg: &str) -> String {
    let trimmed = msg.split('\n').next().unwrap_or(msg).trim();
    let max = 120;
    if trimmed.len() > max {
        format!("{}…", &trimmed[..max])
    } else {
        trimmed.to_string()
    }
}

#[derive(Default)]
struct Counters {
    total: usize,
    no_selection: usize,
    none: usize,
    ast_differs: usize,
    v04_only: usize,
    v03_only: usize,
    both_reject: usize,
    diff_kind_totals: BTreeMap<String, usize>,
    records_with_breaking_flip: usize,
}

impl Counters {
    fn tally(&mut self, v: &Verdict) {
        self.total += 1;
        match v.divergence {
            Divergence::None => self.none += 1,
            Divergence::AstDiffers => self.ast_differs += 1,
            Divergence::V04OnlyAccepts => self.v04_only += 1,
            Divergence::V03OnlyAccepts => self.v03_only += 1,
            Divergence::BothReject => self.both_reject += 1,
            Divergence::NoSelection => self.no_selection += 1,
        }
        let mut had_breaking = false;
        for kind in &v.diff_kinds {
            let tag = diff_tag(kind);
            *self.diff_kind_totals.entry(tag.to_string()).or_insert(0) += 1;
            if is_breaking_tag(tag) {
                had_breaking = true;
            }
        }
        if had_breaking {
            self.records_with_breaking_flip += 1;
        }
    }

    fn report(&self) {
        eprintln!(
            "records: {total}  none: {none}  ast_differs: {differs}  \
             v04_only_accepts: {v04}  v03_only_accepts: {v03}  \
             both_reject: {both}  no_selection: {ns}",
            total = self.total,
            none = self.none,
            differs = self.ast_differs,
            v04 = self.v04_only,
            v03 = self.v03_only,
            both = self.both_reject,
            ns = self.no_selection,
        );
        eprintln!(
            "records with >=1 breaking-flip diff_kind: {n}",
            n = self.records_with_breaking_flip
        );
        eprintln!("diff_kind totals:");
        for (tag, count) in &self.diff_kind_totals {
            eprintln!("  {tag:>40}  {count}");
        }
    }
}

fn diff_tag(kind: &DiffKind) -> &'static str {
    match kind {
        DiffKind::KeyFlippedToLiteralNull { .. } => "key_flipped_to_literal_null",
        DiffKind::KeyFlippedToLiteralBool { .. } => "key_flipped_to_literal_bool",
        DiffKind::KeyFieldFlippedToLiteralString { .. } => "key_field_flipped_to_literal_string",
        DiffKind::KeyQuotedFlippedToLiteralString { .. } => "key_quoted_flipped_to_literal_string",
        DiffKind::SubSelectionToLitObject { .. } => "subselection_to_litobject",
        DiffKind::LegacyObjectToLitObject { .. } => "legacy_object_to_litobject",
        DiffKind::Other { .. } => "other",
    }
}

fn is_breaking_tag(tag: &str) -> bool {
    matches!(
        tag,
        "key_flipped_to_literal_null"
            | "key_flipped_to_literal_bool"
            | "key_field_flipped_to_literal_string"
            | "key_quoted_flipped_to_literal_string"
    )
}
