//! String unescaping and escaping helpers.
//!
//! Escape syntax, surrogate pairing, and UTF-8 are validated during parse, so
//! unescaping a stored span cannot fail.

use crate::simd::{StringScan, scan_string_content};
use crate::utf8::ValidatedUtf8;

/// Unescapes validated string content (the bytes between the quotes).
pub(crate) fn unescape(raw: ValidatedUtf8<'_>) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        if raw[i] != b'\\' {
            // Copy the maximal escape-free run in one step; backslashes are
            // ASCII, so the run falls on character boundaries.
            let start = i;
            while i < raw.len() && raw[i] != b'\\' {
                i += 1;
            }
            out.push_str(raw.slice(start..i).as_str());
            continue;
        }
        i += 1;
        match raw[i] {
            b'"' => out.push('"'),
            b'\\' => out.push('\\'),
            b'/' => out.push('/'),
            b'b' => out.push('\u{0008}'),
            b'f' => out.push('\u{000C}'),
            b'n' => out.push('\n'),
            b'r' => out.push('\r'),
            b't' => out.push('\t'),
            b'u' => {
                let unit = hex4(&raw[i + 1..=i + 4]);
                i += 4;
                if (0xD800..0xDC00).contains(&unit) {
                    // Validated surrogate pair: `\uXXXX\uXXXX`.
                    let low = hex4(&raw[i + 3..=i + 6]);
                    i += 6;
                    let c = 0x10000 + ((unit - 0xD800) << 10) + (low - 0xDC00);
                    out.push(char::from_u32(c).expect("validated surrogate pair"));
                } else {
                    out.push(char::from_u32(unit).expect("validated escape"));
                }
            }
            _ => unreachable!("escape validated at parse"),
        }
        i += 1;
    }
    out
}

fn hex4(digits: &[u8]) -> u32 {
    digits.iter().fold(0, |acc, &d| {
        acc * 16 + (d as char).to_digit(16).expect("validated hex digit")
    })
}

/// Appends string content (no surrounding quotes) as JSON, escaping as
/// `serde_json` does: `"`, `\`, and control characters; everything else
/// passes through. `bytes` must be UTF-8 — arena-owned text always is,
/// having been written from `str` — so no re-validation happens here.
///
/// The bytes that need escaping are exactly the ones the parser's string
/// scanner stops at, so the vector scanner finds each escape-free run.
pub(crate) fn escape_into(bytes: &[u8], out: &mut Vec<u8>) {
    let mut rest = bytes;
    loop {
        let i = match scan_string_content(rest) {
            StringScan::Quote(i) | StringScan::Escape(i) | StringScan::Control(i) => i,
            StringScan::End => {
                out.extend_from_slice(rest);
                return;
            }
        };
        out.extend_from_slice(&rest[..i]);
        push_escape(rest[i], out);
        rest = &rest[i + 1..];
    }
}

/// Appends the JSON escape for `byte`, which is `"`, `\`, or below 0x20.
fn push_escape(byte: u8, out: &mut Vec<u8>) {
    let escape: &[u8] = match byte {
        b'"' => b"\\\"",
        b'\\' => b"\\\\",
        0x08 => b"\\b",
        0x0C => b"\\f",
        b'\n' => b"\\n",
        b'\r' => b"\\r",
        b'\t' => b"\\t",
        _ => {
            const HEX: &[u8; 16] = b"0123456789abcdef";
            out.extend_from_slice(b"\\u00");
            out.push(HEX[(byte >> 4) as usize]);
            out.push(HEX[(byte & 0xF) as usize]);
            return;
        }
    };
    out.extend_from_slice(escape);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unescape_handles_all_escape_forms() {
        assert_eq!(
            unescape(r#"a\"b\\c\/d\b\f\n\r\t"#.into()),
            "a\"b\\c/d\u{8}\u{c}\n\r\t"
        );
        assert_eq!(unescape(r"a\u00e9b".into()), "a\u{e9}b");
        assert_eq!(unescape(r"\ud83d\ude00".into()), "\u{1F600}");
        assert_eq!(unescape("héllo中文".into()), "héllo中文");
    }

    #[test]
    fn escape_matches_serde_json() {
        for text in [
            "plain",
            "q\"b\\s",
            "\u{1}\u{8}\u{c}\n\r\t\u{1f}",
            "héllo中文😀",
        ] {
            let mut out = Vec::new();
            escape_into(text.as_bytes(), &mut out);
            let expected = serde_json::to_string(text).unwrap();
            assert_eq!(
                String::from_utf8(out).unwrap(),
                expected[1..expected.len() - 1],
            );
        }
    }

    fn serde_escaped(text: &str) -> String {
        let quoted = serde_json::to_string(text).unwrap();
        quoted[1..quoted.len() - 1].to_string()
    }

    fn escaped(text: &str) -> String {
        let mut out = Vec::new();
        escape_into(text.as_bytes(), &mut out);
        String::from_utf8(out).unwrap()
    }

    /// Every byte that needs escaping, at every position of strings long
    /// enough to reach each vector block size and the scalar tail, written
    /// once and as a run of three.
    #[test]
    fn escapes_found_at_every_position() {
        for special in [
            '"', '\\', '\u{0}', '\u{8}', '\u{c}', '\n', '\r', '\t', '\u{1f}',
        ] {
            for len in [1usize, 15, 16, 17, 31, 32, 33, 63, 64, 65, 97, 130] {
                for pos in 0..len {
                    for run in [1, 3] {
                        let mut text = "x".repeat(len);
                        text.replace_range(pos..pos + 1, &special.to_string().repeat(run));
                        assert_eq!(escaped(&text), serde_escaped(&text), "{special:?} at {pos}");
                    }
                }
            }
        }
    }

    /// Multibyte UTF-8 passes through unchanged next to escapes, across
    /// block boundaries.
    #[test]
    fn multibyte_text_passes_through() {
        for prefix in 0..40 {
            let text = format!(
                "{}héllo\"中文\\😀\n{}",
                "a".repeat(prefix),
                "é".repeat(prefix)
            );
            assert_eq!(escaped(&text), serde_escaped(&text), "prefix {prefix}");
        }
        assert_eq!(escaped(""), "");
    }
}
