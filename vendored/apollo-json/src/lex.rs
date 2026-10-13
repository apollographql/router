//! Byte-level JSON lexing shared by the parser and the streaming
//! deserializer.
//!
//! The lexer owns the cursor over the input and knows the token grammar —
//! whitespace runs, string content with escape validation, number literals —
//! but nothing about containers or where the pieces go. The input arrives as
//! [`ValidatedUtf8`], so every range the lexer hands back is trusted UTF-8:
//! quotes are ASCII and cannot split a multi-byte character.

use std::ops::Range;

use crate::error::JsonError;
use crate::utf8::ValidatedUtf8;

pub(crate) struct Lexer<'a> {
    input: ValidatedUtf8<'a>,
    pos: usize,
}

impl<'a> Lexer<'a> {
    pub(crate) fn new(input: ValidatedUtf8<'a>) -> Self {
        Lexer { input, pos: 0 }
    }

    pub(crate) fn input(&self) -> ValidatedUtf8<'a> {
        self.input
    }

    pub(crate) fn pos(&self) -> usize {
        self.pos
    }

    pub(crate) fn at_end(&self) -> bool {
        self.pos == self.input.len()
    }

    pub(crate) fn peek(&self) -> Result<u8, JsonError> {
        self.input.get(self.pos).copied().ok_or(JsonError::Syntax {
            offset: self.pos,
            reason: "unexpected end of input",
        })
    }

    pub(crate) fn next(&mut self) -> Result<u8, JsonError> {
        let b = self.peek()?;
        self.pos += 1;
        Ok(b)
    }

    /// Advances past the byte at the cursor (already inspected via `peek`).
    pub(crate) fn bump(&mut self) {
        self.pos += 1;
    }

    /// Advances past `n` bytes consumed outside the lexer (a delegated
    /// sub-parse).
    pub(crate) fn advance(&mut self, n: usize) {
        self.pos += n;
    }

    pub(crate) fn syntax(&self, reason: &'static str) -> JsonError {
        JsonError::Syntax {
            offset: self.pos,
            reason,
        }
    }

    /// Syntax error pointing at the byte just consumed.
    pub(crate) fn syntax_before(&self, reason: &'static str) -> JsonError {
        JsonError::Syntax {
            offset: self.pos.saturating_sub(1),
            reason,
        }
    }

    /// Advances past whitespace. Minified traffic almost never has any, so
    /// the check stays small enough to inline; runs are handled by the
    /// word-at-a-time slow path.
    #[inline]
    pub(crate) fn skip_ws(&mut self) {
        if matches!(self.input.get(self.pos), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.skip_ws_run();
        }
    }

    /// Skips a whitespace run eight bytes at a time (SWAR), with a scalar
    /// tail. The per-byte flags of `zero_bytes` are exact, so the computed
    /// stop position needs no re-check.
    #[inline(never)]
    fn skip_ws_run(&mut self) {
        self.pos += 1;
        const HIGH: u64 = 0x8080_8080_8080_8080;
        while self.pos + 8 <= self.input.len() {
            let chunk = u64::from_le_bytes(
                self.input[self.pos..self.pos + 8]
                    .try_into()
                    .expect("slice is eight bytes"),
            );
            let ws = zero_bytes(chunk ^ 0x2020_2020_2020_2020)
                | zero_bytes(chunk ^ 0x0909_0909_0909_0909)
                | zero_bytes(chunk ^ 0x0A0A_0A0A_0A0A_0A0A)
                | zero_bytes(chunk ^ 0x0D0D_0D0D_0D0D_0D0D);
            if ws == HIGH {
                self.pos += 8;
                continue;
            }
            self.pos += ((ws ^ HIGH).trailing_zeros() / 8) as usize;
            return;
        }
        while matches!(self.input.get(self.pos), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    /// Scans a string with the cursor on the opening quote, returning the
    /// content range (escapes intact) and whether it contains any.
    ///
    /// The vectorized scanner classifies quote / escape / control bytes in
    /// one pass; short strings resolve in a single vector block.
    pub(crate) fn scan_string(&mut self) -> Result<(Range<usize>, bool), JsonError> {
        self.pos += 1;
        let start = self.pos;
        let mut escaped = false;
        loop {
            match crate::simd::scan_string_content(&self.input[self.pos..]) {
                crate::simd::StringScan::Quote(i) => {
                    self.pos += i;
                    break;
                }
                crate::simd::StringScan::Escape(i) => {
                    escaped = true;
                    self.pos += i + 1;
                    self.scan_escape()?;
                }
                crate::simd::StringScan::Control(i) => {
                    self.pos += i;
                    return Err(self.syntax("unescaped control character in string"));
                }
                crate::simd::StringScan::End => {
                    self.pos = self.input.len();
                    return Err(self.syntax("unterminated string"));
                }
            }
        }
        let range = start..self.pos;
        self.pos += 1;
        Ok((range, escaped))
    }

    /// Validates one escape sequence with the cursor on the byte after `\`.
    fn scan_escape(&mut self) -> Result<(), JsonError> {
        match self.next()? {
            b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't' => Ok(()),
            b'u' => {
                let unit = self.scan_hex4()?;
                if (0xDC00..0xE000).contains(&unit) {
                    return Err(self.syntax("unpaired surrogate escape"));
                }
                if (0xD800..0xDC00).contains(&unit) {
                    if self.next()? != b'\\' || self.next()? != b'u' {
                        return Err(self.syntax("unpaired surrogate escape"));
                    }
                    let low = self.scan_hex4()?;
                    if !(0xDC00..0xE000).contains(&low) {
                        return Err(self.syntax("unpaired surrogate escape"));
                    }
                }
                Ok(())
            }
            _ => Err(self.syntax_before("invalid escape sequence")),
        }
    }

    fn scan_hex4(&mut self) -> Result<u32, JsonError> {
        let mut unit = 0u32;
        for _ in 0..4 {
            let digit = (self.next()? as char)
                .to_digit(16)
                .ok_or_else(|| self.syntax_before("invalid \\u escape"))?;
            unit = unit * 16 + digit;
        }
        Ok(unit)
    }

    /// Scans a number literal, validating the JSON grammar without decoding.
    pub(crate) fn scan_number(&mut self) -> Result<Range<usize>, JsonError> {
        let start = self.pos;
        if self.input[self.pos] == b'-' {
            self.pos += 1;
        }
        match self.input.get(self.pos) {
            Some(b'0') => self.pos += 1,
            Some(b'1'..=b'9') => self.skip_digits(),
            _ => return Err(self.syntax("invalid number")),
        }
        if self.input.get(self.pos) == Some(&b'.') {
            self.pos += 1;
            if !matches!(self.input.get(self.pos), Some(b'0'..=b'9')) {
                return Err(self.syntax("expected digits after decimal point"));
            }
            self.skip_digits();
        }
        if matches!(self.input.get(self.pos), Some(b'e' | b'E')) {
            self.pos += 1;
            if matches!(self.input.get(self.pos), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            if !matches!(self.input.get(self.pos), Some(b'0'..=b'9')) {
                return Err(self.syntax("expected digits in exponent"));
            }
            self.skip_digits();
        }
        Ok(start..self.pos)
    }

    /// Advances past a digit run. The scalar loop handles the short runs
    /// that dominate real documents; a run reaching eight digits falls
    /// through to the word-at-a-time loop.
    #[inline]
    fn skip_digits(&mut self) {
        let start = self.pos;
        while matches!(self.input.get(self.pos), Some(b'0'..=b'9')) {
            self.pos += 1;
            if self.pos - start == 8 {
                self.skip_digits_run();
                return;
            }
        }
    }

    /// SWAR continuation for long digit runs; the scalar tail re-checks from
    /// the first flagged byte, so the conservative carry behavior of the
    /// word trick cannot skip a non-digit.
    #[inline(never)]
    fn skip_digits_run(&mut self) {
        while self.pos + 8 <= self.input.len() {
            let chunk = u64::from_le_bytes(
                self.input[self.pos..self.pos + 8]
                    .try_into()
                    .expect("slice is eight bytes"),
            );
            // Digits become 0x00..=0x09; adding 0x76 sets a byte's high bit
            // for values above 9 (carries between bytes only over-flag).
            let x = chunk ^ 0x3030_3030_3030_3030;
            let non_digit = (x.wrapping_add(0x7676_7676_7676_7676) | x) & 0x8080_8080_8080_8080;
            if non_digit == 0 {
                self.pos += 8;
                continue;
            }
            self.pos += (non_digit.trailing_zeros() / 8) as usize;
            break;
        }
        while matches!(self.input.get(self.pos), Some(b'0'..=b'9')) {
            self.pos += 1;
        }
    }

    pub(crate) fn expect_literal(&mut self, literal: &'static [u8]) -> Result<(), JsonError> {
        if self.input[self.pos..].starts_with(literal) {
            self.pos += literal.len();
            Ok(())
        } else {
            Err(self.syntax("invalid literal"))
        }
    }
}

/// 0x80 in each byte of `v` that is zero, 0x00 elsewhere. Exact per byte:
/// `(v & 0x7F) + 0x7F` cannot carry across byte lanes.
#[inline]
fn zero_bytes(v: u64) -> u64 {
    const LOW7: u64 = 0x7F7F_7F7F_7F7F_7F7F;
    !(((v & LOW7) + LOW7) | v | LOW7)
}
