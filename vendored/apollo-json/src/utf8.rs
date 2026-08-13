//! UTF-8 validity carried in the type system.
//!
//! Stored spans and owned text are read as `&str` without re-validation
//! (re-validating on every access was ~30% of typed-deserialization self
//! time). The types here make the unchecked conversion sound by
//! construction rather than by caller discipline: bytes only become
//! [`ValidatedUtf8`] through this module's constructors — whole-input
//! validation, copies of `&str`, or slices of already-validated text — so
//! the proof obligation lives entirely here. Debug builds re-check on
//! every conversion.

use std::ops::Deref;
use std::slice::SliceIndex;

use bytes::Bytes;

use crate::error::JsonError;
use crate::slab::{SlabRef, Slabs};

/// Validates `input`, reporting the offset of the first invalid byte.
///
/// This is the whole-input validation the parser and the streaming
/// deserializer run up front; every span they hand out afterwards falls on
/// ASCII token boundaries, so slices of the result stay valid.
pub(crate) fn validate_utf8(input: &[u8]) -> Result<ValidatedUtf8<'_>, JsonError> {
    check(input)?;
    Ok(ValidatedUtf8(input))
}

fn check(input: &[u8]) -> Result<(), JsonError> {
    if simdutf8::basic::from_utf8(input).is_err() {
        let offset = simdutf8::compat::from_utf8(input)
            .expect_err("basic validation already failed")
            .valid_up_to();
        return Err(JsonError::Syntax {
            offset,
            reason: "invalid UTF-8",
        });
    }
    Ok(())
}

/// A byte slice proven valid UTF-8 at construction, convertible to `&str`
/// without re-validation.
#[derive(Clone, Copy)]
pub(crate) struct ValidatedUtf8<'a>(&'a [u8]);

impl<'a> ValidatedUtf8<'a> {
    /// The bytes as `&str`. Debug builds re-check the invariant.
    pub(crate) fn as_str(self) -> &'a str {
        debug_assert!(
            std::str::from_utf8(self.0).is_ok(),
            "validated bytes must be UTF-8"
        );
        // SAFETY: the wrapped bytes are valid UTF-8 by construction — every
        // constructor either validates the whole buffer, copies from `&str`,
        // or slices already-validated text at ASCII boundaries.
        unsafe { std::str::from_utf8_unchecked(self.0) }
    }

    /// The bytes with the original lifetime (unlike `Deref`, which ties
    /// them to the borrow of `self`).
    pub(crate) fn as_bytes(self) -> &'a [u8] {
        self.0
    }

    /// A sub-slice.
    ///
    /// # Panics
    ///
    /// Panics if either end of `range` is out of bounds or falls inside a
    /// multi-byte character — a cut that splits a character would make
    /// [`as_str`](Self::as_str) undefined behaviour, so both ends are
    /// checked in every build. Every range the lexer produces is aligned,
    /// because JSON delimiters (quotes, escapes, digits) are ASCII.
    pub(crate) fn slice<R>(self, range: R) -> ValidatedUtf8<'a>
    where
        R: SliceIndex<[u8], Output = [u8]>,
    {
        let bytes = &self.0[range];
        // Offsets recovered from the sub-slice so the check works for every
        // range form. A position is a boundary when it is one-past-the-end
        // or its byte is not a UTF-8 continuation byte.
        let start = bytes.as_ptr() as usize - self.0.as_ptr() as usize;
        let end = start + bytes.len();
        let boundary = |i: usize| i == self.0.len() || (self.0[i] as i8) >= -0x40;
        assert!(
            boundary(start) && boundary(end),
            "slice must cut on character boundaries"
        );
        ValidatedUtf8(bytes)
    }

    /// Clamps to at most `max` bytes, backing off past any multi-byte
    /// character the cut would split so the view stays valid UTF-8.
    pub(crate) fn truncate(self, max: usize) -> ValidatedUtf8<'a> {
        if self.0.len() <= max {
            return self;
        }
        let text = self.as_str();
        let mut end = max;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        ValidatedUtf8(&self.0[..end])
    }
}

impl<'a> From<&'a str> for ValidatedUtf8<'a> {
    fn from(text: &'a str) -> Self {
        ValidatedUtf8(text.as_bytes())
    }
}

impl Deref for ValidatedUtf8<'_> {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        self.0
    }
}

/// An owned buffer of validated UTF-8: the arena's input storage.
pub(crate) struct Utf8Bytes(Bytes);

impl Utf8Bytes {
    pub(crate) fn new() -> Self {
        Utf8Bytes(Bytes::new())
    }

    /// Validates a whole parse input and takes ownership of it. A caller
    /// already holding [`Bytes`] — an HTTP body, say — hands over a refcount,
    /// not a copy.
    pub(crate) fn validate(input: Bytes) -> Result<Self, JsonError> {
        check(&input)?;
        Ok(Utf8Bytes(input))
    }

    /// Copies already-validated text (a capture's consumed prefix).
    pub(crate) fn copy_of(text: ValidatedUtf8<'_>) -> Self {
        Utf8Bytes(Bytes::copy_from_slice(&text))
    }

    /// The whole buffer as validated text.
    pub(crate) fn utf8(&self) -> ValidatedUtf8<'_> {
        ValidatedUtf8(&self.0)
    }

    /// The buffer as a shareable handle, for zero-copy slices.
    pub(crate) fn as_shared(&self) -> &Bytes {
        &self.0
    }
}

/// A detached input assembles as `String` — pieces arrive as validated
/// text — so the finished buffer is valid by the same construction.
impl From<String> for Utf8Bytes {
    fn from(input: String) -> Self {
        Utf8Bytes(Bytes::from(input))
    }
}

impl Deref for Utf8Bytes {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        &self.0
    }
}

/// Slab storage for owned text: only `&str` goes in, so reads come back
/// as validated text.
pub(crate) struct Utf8Slabs(Slabs<u8>);

impl Utf8Slabs {
    pub(crate) fn new() -> Self {
        Utf8Slabs(Slabs::new())
    }

    /// Copies `text` into the slab.
    pub(crate) fn alloc(&mut self, text: &str) -> SlabRef {
        self.0.alloc(text.as_bytes())
    }

    pub(crate) fn get(&self, slab: SlabRef) -> ValidatedUtf8<'_> {
        ValidatedUtf8(self.0.get(slab))
    }

    /// Approximate bytes retained.
    pub(crate) fn bytes(&self) -> usize {
        self.0.bytes()
    }

    /// Empties the storage, keeping its capacity.
    pub(crate) fn reset(&mut self) {
        self.0.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::ValidatedUtf8;

    #[test]
    fn slice_accepts_character_boundaries() {
        let text = ValidatedUtf8::from("a€b");
        assert_eq!(text.slice(1..4).as_str(), "€");
    }

    #[test]
    #[should_panic(expected = "character boundaries")]
    fn slice_rejects_a_cut_inside_a_character() {
        let text = ValidatedUtf8::from("a€b");
        let _ = text.slice(1..2);
    }
}
