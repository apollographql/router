//! Iterative JSON parser.
//!
//! A single pass over the input with an explicit container stack — never
//! recursive, so nesting depth is bounded by [`ParseOptions`], not the thread
//! stack. Leaf scalars are recorded as spans into the input; strings and
//! escapes are validated here so lazy access cannot fail later. The
//! byte-level tokens come from the shared [`Lexer`].
//!
//! Container children accumulate in reusable scratch stacks (one for array
//! children, one for object entries); when a container closes, its tail range
//! is copied into an arena slab and the scratch truncates. A parse therefore
//! performs no per-container heap allocation — heap traffic is the arena's
//! fixed-size chunks plus the handful of scratch buffers.

use std::borrow::Cow;
use std::ops::Range;

use ahash::AHashMap;

use crate::arena::{Arena, estimate_nodes};
use crate::error::JsonError;
use crate::lex::Lexer;
use crate::node::{Child, Entry, KeyRef, Node, NodeId, Span};
use crate::options::ParseOptions;
use bytes::Bytes;

use crate::utf8::{Utf8Bytes, ValidatedUtf8};

pub(crate) struct Parsed {
    pub(crate) arena: Arena,
    pub(crate) root: NodeId,
}

/// Reusable parse storage for a parse-and-drop loop.
///
/// A parse allocates arena chunks and scratch buffers; recycling them
/// through a `ParseBuffers` makes steady-state parses nearly allocation-
/// free. Pass the buffers to [`Value::parse_with_buffers`] and hand a
/// finished document's storage back with [`Value::recycle`]:
///
/// ```
/// use apollo_json::{ParseBuffers, ParseOptions, Value};
///
/// let options = ParseOptions::default();
/// let mut buffers = ParseBuffers::new();
/// for _ in 0..3 {
///     let input = br#"{"user":{"id":1}}"#.to_vec();
///     let doc = Value::parse_with_buffers(input, &options, &mut buffers)?;
///     let response = doc.to_vec();
///     doc.recycle(&mut buffers); // storage backs the next iteration
///     assert_eq!(response, br#"{"user":{"id":1}}"#);
/// }
/// # Ok::<(), apollo_json::JsonError>(())
/// ```
///
/// The retained storage keeps its high-water capacity, bounded by the
/// arena-size cap of the parses that filled it; drop the buffers to release
/// it.
///
/// [`Value::parse_with_buffers`]: crate::Value::parse_with_buffers
/// [`Value::recycle`]: crate::Value::recycle
#[derive(Default)]
pub struct ParseBuffers {
    pub(crate) arena: Option<Arena>,
    child_scratch: Vec<Child>,
    entry_scratch: Vec<Entry>,
}

impl ParseBuffers {
    /// Empty buffers; the first parse allocates and later ones reuse.
    pub fn new() -> Self {
        Self::default()
    }
}

impl std::fmt::Debug for ParseBuffers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ParseBuffers").finish_non_exhaustive()
    }
}

pub(crate) fn parse(input: Bytes, options: &ParseOptions) -> Result<Parsed, JsonError> {
    parse_with(input, options, &mut ParseBuffers::new())
}

/// Parses the first complete JSON value of `input` into its own arena,
/// returning the consumed byte length. The streaming deserializer uses this
/// to capture one subtree mid-stream: spans come out relative to `input`'s
/// start, and the arena retains a copy of exactly the consumed bytes.
///
/// `buffers` supplies scratch storage only; the arena is always fresh
/// because a capture keeps it. Syntax-error offsets are relative to
/// `input`'s start.
pub(crate) fn parse_prefix(
    input: ValidatedUtf8<'_>,
    options: &ParseOptions,
    buffers: &mut ParseBuffers,
) -> Result<(Parsed, usize), JsonError> {
    let effective_cap = options.max_arena_bytes.min(u32::MAX as usize);
    // Clamp what the lexer can see so a single oversized token cannot push
    // spans past u32; a value that large fails the arena cap anyway, and so
    // does one reaching the few extra bytes a character-boundary back-off
    // may shave.
    let clamped = input.len() > effective_cap.saturating_add(1);
    let input = input.truncate(effective_cap.saturating_add(1));
    let mut parser = Parser {
        lex: Lexer::new(input),
        // The value's length is unknown until it closes; start minimal and
        // let the chunk ramping absorb larger subtrees.
        arena: Arena::new(crate::arena::DEFAULT_NODE_ESTIMATE),
        stack: Vec::new(),
        child_scratch: std::mem::take(&mut buffers.child_scratch),
        entry_scratch: std::mem::take(&mut buffers.entry_scratch),
        max_depth: options.max_depth,
        max_bytes: effective_cap,
        // Only the consumed prefix is retained, not the whole slice.
        retained_input: RetainedInput::Consumed,
    };
    let root = parser.parse_value();
    parser.child_scratch.clear();
    parser.entry_scratch.clear();
    buffers.child_scratch = std::mem::take(&mut parser.child_scratch);
    buffers.entry_scratch = std::mem::take(&mut parser.entry_scratch);
    let root = match root {
        Ok(root) => root,
        // An error at the end of the clamped view means the value reached
        // past the arena cap, not that the (longer) input is malformed:
        // report the limit, not a phantom truncation.
        Err(JsonError::Syntax { offset, .. }) if clamped && offset >= input.len() => {
            return Err(JsonError::ArenaLimitExceeded {
                limit: options.max_arena_bytes,
            });
        }
        Err(error) => return Err(error),
    };
    let root = root.as_local().expect("parse only creates local nodes");
    let consumed = parser.lex.pos();
    let mut arena = parser.arena;
    if arena.bytes() + consumed > effective_cap {
        return Err(JsonError::ArenaLimitExceeded {
            limit: options.max_arena_bytes,
        });
    }
    arena.set_input(Utf8Bytes::copy_of(input.slice(..consumed)));
    Ok((Parsed { arena, root }, consumed))
}

pub(crate) fn parse_with(
    input: Bytes,
    options: &ParseOptions,
    buffers: &mut ParseBuffers,
) -> Result<Parsed, JsonError> {
    // Spans are u32 offsets; the arena cap keeps inputs within range.
    let effective_cap = options.max_arena_bytes.min(u32::MAX as usize);
    if input.len() > effective_cap {
        return Err(JsonError::ArenaLimitExceeded {
            limit: options.max_arena_bytes,
        });
    }
    // Validate UTF-8 once for the whole input. Every string span is then
    // guaranteed valid: quotes are ASCII, so a span boundary can never fall
    // inside a multi-byte character. Bytes outside strings are constrained
    // to ASCII by the grammar anyway.
    let input = Utf8Bytes::validate(input)?;
    // A recycled arena's retained capacity counts against the cap; discard
    // it rather than fail a parse a fresh arena would accept.
    let arena = match buffers.arena.take() {
        Some(arena) if arena.bytes() + input.len() <= effective_cap => arena,
        _ => Arena::new(estimate_nodes(input.len())),
    };
    let mut parser = Parser {
        lex: Lexer::new(input.utf8()),
        arena,
        stack: Vec::new(),
        child_scratch: std::mem::take(&mut buffers.child_scratch),
        entry_scratch: std::mem::take(&mut buffers.entry_scratch),
        max_depth: options.max_depth,
        max_bytes: effective_cap,
        retained_input: RetainedInput::Whole,
    };
    let root = parser.parse_document();
    // Scratch goes back to the caller whether or not the parse succeeded.
    parser.child_scratch.clear();
    parser.entry_scratch.clear();
    buffers.child_scratch = std::mem::take(&mut parser.child_scratch);
    buffers.entry_scratch = std::mem::take(&mut parser.entry_scratch);
    let root = root?.as_local().expect("parse only creates local nodes");
    let mut arena = parser.arena;
    arena.set_input(input);
    Ok(Parsed { arena, root })
}

/// Object width at which duplicate-key detection switches from a linear
/// scan to a hashed index; the scan is quadratic in object width.
pub(crate) const INDEX_THRESHOLD: usize = 32;

enum Frame<'a> {
    /// An open array; children from `start` in the child scratch belong to
    /// it.
    Array { start: usize },
    /// An open object; entries from `start` in the entry scratch belong to
    /// it.
    Object {
        start: usize,
        /// Unescaped key text to entry offset (relative to `start`), built
        /// once the object grows past [`INDEX_THRESHOLD`]. Uses `ahash` —
        /// 2-5x faster than SipHash on short keys; collision-DoS hardness is
        /// not load-bearing for a per-object, arena-capped index.
        index: Option<AHashMap<Cow<'a, [u8]>, u32>>,
        /// The key parsed for the value currently being read.
        pending: Option<KeyRef>,
    },
}

/// How much of the lexer's input the finished arena will retain, for the
/// running arena-cap check.
enum RetainedInput {
    /// A whole-document parse keeps the full input.
    Whole,
    /// A prefix parse keeps only the bytes consumed so far.
    Consumed,
}

struct Parser<'a> {
    lex: Lexer<'a>,
    arena: Arena,
    stack: Vec<Frame<'a>>,
    /// Children of every open array, contiguous per frame.
    child_scratch: Vec<Child>,
    /// Entries of every open object, contiguous per frame.
    entry_scratch: Vec<Entry>,
    max_depth: usize,
    max_bytes: usize,
    retained_input: RetainedInput,
}

/// A lexer range as an arena span; the arena cap keeps offsets within `u32`.
fn span(range: Range<usize>) -> Span {
    Span {
        start: u32::try_from(range.start).expect("input within span range"),
        len: u32::try_from(range.len()).expect("input within span range"),
    }
}

impl Parser<'_> {
    fn parse_document(&mut self) -> Result<Child, JsonError> {
        let root = self.parse_value()?;
        self.lex.skip_ws();
        if !self.lex.at_end() {
            return Err(self.lex.syntax("trailing characters after the document"));
        }
        Ok(root)
    }

    /// The main loop: reads one value, then attaches it to the enclosing
    /// container and unwinds any containers that close.
    fn parse_value(&mut self) -> Result<Child, JsonError> {
        'value: loop {
            self.lex.skip_ws();
            let mut completed = match self.lex.peek()? {
                b'{' => {
                    self.lex.bump();
                    self.check_depth()?;
                    self.lex.skip_ws();
                    if self.lex.peek()? == b'}' {
                        self.lex.bump();
                        self.push(Node::Object(crate::slab::SlabRef::EMPTY))?
                    } else {
                        let pending = Some(self.parse_key()?);
                        self.stack.push(Frame::Object {
                            start: self.entry_scratch.len(),
                            index: None,
                            pending,
                        });
                        continue 'value;
                    }
                }
                b'[' => {
                    self.lex.bump();
                    self.check_depth()?;
                    self.lex.skip_ws();
                    if self.lex.peek()? == b']' {
                        self.lex.bump();
                        self.push(Node::Array(crate::slab::SlabRef::EMPTY))?
                    } else {
                        self.stack.push(Frame::Array {
                            start: self.child_scratch.len(),
                        });
                        continue 'value;
                    }
                }
                b'"' => {
                    let (range, escaped) = self.lex.scan_string()?;
                    self.push(Node::String {
                        span: span(range),
                        escaped,
                    })?
                }
                b'-' | b'0'..=b'9' => {
                    let range = self.lex.scan_number()?;
                    self.push(Node::Number(span(range)))?
                }
                b't' => {
                    self.lex.expect_literal(b"true")?;
                    self.push(Node::Bool(true))?
                }
                b'f' => {
                    self.lex.expect_literal(b"false")?;
                    self.push(Node::Bool(false))?
                }
                b'n' => {
                    self.lex.expect_literal(b"null")?;
                    self.push(Node::Null)?
                }
                _ => return Err(self.lex.syntax("expected a JSON value")),
            };

            loop {
                if self.stack.is_empty() {
                    return Ok(completed);
                }
                let in_object = {
                    let input = self.lex.input();
                    match self.stack.last_mut().expect("stack checked non-empty") {
                        Frame::Array { .. } => {
                            self.child_scratch.push(completed);
                            false
                        }
                        Frame::Object {
                            start,
                            index,
                            pending,
                        } => {
                            let key = pending.take().expect("a value always follows a key");
                            insert_entry(
                                input,
                                &mut self.entry_scratch,
                                *start,
                                index,
                                key,
                                completed,
                            );
                            true
                        }
                    }
                };
                self.lex.skip_ws();
                match (self.lex.next()?, in_object) {
                    (b',', false) => continue 'value,
                    (b',', true) => {
                        self.lex.skip_ws();
                        let key = self.parse_key()?;
                        let Some(Frame::Object { pending, .. }) = self.stack.last_mut() else {
                            unreachable!("top frame is an object");
                        };
                        *pending = Some(key);
                        continue 'value;
                    }
                    (b']', false) => {
                        let Some(Frame::Array { start }) = self.stack.pop() else {
                            unreachable!("top frame is an array");
                        };
                        let slab = self.arena.alloc_children(&self.child_scratch[start..]);
                        self.child_scratch.truncate(start);
                        completed = self.push(Node::Array(slab))?;
                    }
                    (b'}', true) => {
                        let Some(Frame::Object { start, .. }) = self.stack.pop() else {
                            unreachable!("top frame is an object");
                        };
                        let slab = self.arena.alloc_entries(&self.entry_scratch[start..]);
                        self.entry_scratch.truncate(start);
                        completed = self.push(Node::Object(slab))?;
                    }
                    (_, false) => return Err(self.lex.syntax_before("expected ',' or ']'")),
                    (_, true) => return Err(self.lex.syntax_before("expected ',' or '}'")),
                }
            }
        }
    }

    /// Parses `"key" ws :` with the cursor on the opening quote.
    fn parse_key(&mut self) -> Result<KeyRef, JsonError> {
        if self.lex.peek()? != b'"' {
            return Err(self.lex.syntax("expected an object key"));
        }
        let (range, escaped) = self.lex.scan_string()?;
        self.lex.skip_ws();
        if self.lex.next()? != b':' {
            return Err(self.lex.syntax_before("expected ':' after object key"));
        }
        Ok(KeyRef::Span {
            span: span(range),
            escaped,
        })
    }

    fn check_depth(&self) -> Result<(), JsonError> {
        if self.stack.len() + 1 > self.max_depth {
            Err(JsonError::DepthLimitExceeded {
                limit: self.max_depth,
            })
        } else {
            Ok(())
        }
    }

    fn push(&mut self, node: Node) -> Result<Child, JsonError> {
        let id = self.arena.push_node(node);
        let retained = match self.retained_input {
            RetainedInput::Whole => self.lex.input().len(),
            RetainedInput::Consumed => self.lex.pos(),
        };
        if self.arena.bytes() + retained > self.max_bytes {
            return Err(JsonError::ArenaLimitExceeded {
                limit: self.max_bytes,
            });
        }
        Ok(Child::local(id))
    }
}

/// Appends an object entry to the scratch tail beginning at `start`, giving
/// duplicate keys `serde_json` `preserve_order` semantics: the first
/// occurrence keeps its position and spelling, the last value wins.
///
/// Narrow objects scan existing keys directly — the slice comparison
/// short-circuits on length, which is cheaper than hashing every key. Wide
/// objects switch to a hashed index.
fn insert_entry<'a>(
    input: ValidatedUtf8<'a>,
    entries: &mut Vec<Entry>,
    start: usize,
    index: &mut Option<AHashMap<Cow<'a, [u8]>, u32>>,
    key: KeyRef,
    child: Child,
) {
    if let Some(map) = index {
        match map.entry(key_bytes(input, key)) {
            std::collections::hash_map::Entry::Occupied(slot) => {
                entries[start + *slot.get() as usize].child = child;
            }
            std::collections::hash_map::Entry::Vacant(slot) => {
                let offset = entries.len() - start;
                slot.insert(u32::try_from(offset).expect("entry count within range"));
                entries.push(Entry { key, child });
            }
        }
        return;
    }
    for existing in &mut entries[start..] {
        if keys_equal(input, existing.key, key) {
            existing.child = child;
            return;
        }
    }
    entries.push(Entry { key, child });
    if entries.len() - start >= INDEX_THRESHOLD {
        *index = Some(
            entries[start..]
                .iter()
                .enumerate()
                .map(|(i, entry)| {
                    (
                        key_bytes(input, entry.key),
                        u32::try_from(i).expect("entry count within range"),
                    )
                })
                .collect(),
        );
    }
}

/// The key's unescaped text, borrowing the input where possible. Parse-time
/// keys are always spans into the input.
fn key_bytes<'a>(input: ValidatedUtf8<'a>, key: KeyRef) -> Cow<'a, [u8]> {
    match key {
        KeyRef::Span {
            span,
            escaped: false,
        } => Cow::Borrowed(span.slice(input.as_bytes())),
        KeyRef::Span {
            span,
            escaped: true,
        } => Cow::Owned(crate::text::unescape(input.slice(span.range())).into_bytes()),
        KeyRef::Owned(_) => unreachable!("parse only creates span keys"),
    }
}

fn keys_equal(input: ValidatedUtf8<'_>, a: KeyRef, b: KeyRef) -> bool {
    match (key_raw(input, a), key_raw(input, b)) {
        ((a, false), (b, false)) => a == b,
        _ => key_bytes(input, a) == key_bytes(input, b),
    }
}

fn key_raw<'a>(input: ValidatedUtf8<'a>, key: KeyRef) -> (&'a [u8], bool) {
    match key {
        KeyRef::Span { span, escaped } => (span.slice(input.as_bytes()), escaped),
        KeyRef::Owned(_) => unreachable!("parse only creates span keys"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A value larger than the arena cap fails as a limit, not as a syntax
    /// error: the prefix parse clamps how far the lexer can see, and a token
    /// running into the clamp must not read as truncated input.
    #[test]
    fn oversized_prefix_value_reports_the_arena_limit() {
        let options = ParseOptions::default().with_max_arena_bytes(64);
        let mut buffers = ParseBuffers::new();

        // A string token spanning the whole clamped view.
        let input = format!("\"{}\"", "a".repeat(200));
        let error = parse_prefix(input.as_str().into(), &options, &mut buffers)
            .err()
            .expect("parse must fail");
        assert!(
            matches!(error, JsonError::ArenaLimitExceeded { limit: 64 }),
            "{error}"
        );

        // A container left open by the clamp.
        let input = format!("[{}", "1,".repeat(200));
        let error = parse_prefix(input.as_str().into(), &options, &mut buffers)
            .err()
            .expect("parse must fail");
        assert!(
            matches!(error, JsonError::ArenaLimitExceeded { limit: 64 }),
            "{error}"
        );
    }

    /// Genuine syntax errors keep their class and offset even when the input
    /// extends past the clamp.
    #[test]
    fn syntax_errors_before_the_clamp_stay_syntax_errors() {
        let options = ParseOptions::default().with_max_arena_bytes(64);
        let mut buffers = ParseBuffers::new();
        let input = format!("[tru,{}]", "1,".repeat(200));
        let error = parse_prefix(input.as_str().into(), &options, &mut buffers)
            .err()
            .expect("parse must fail");
        assert!(
            matches!(error, JsonError::Syntax { offset: 1, .. }),
            "{error}"
        );
    }

    /// Without clamping, running off the end of the input is still the
    /// syntax error it always was.
    #[test]
    fn truncated_input_within_the_cap_stays_a_syntax_error() {
        let mut buffers = ParseBuffers::new();
        let error = parse_prefix(
            "\"unterminated".into(),
            &ParseOptions::default(),
            &mut buffers,
        )
        .err()
        .expect("parse must fail");
        assert!(matches!(error, JsonError::Syntax { .. }), "{error}");
    }
}
