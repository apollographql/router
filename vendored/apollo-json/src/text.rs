//! String unescaping and escaping helpers.
//!
//! Escape syntax, surrogate pairing, and UTF-8 are validated during parse, so
//! unescaping a stored span cannot fail.

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
pub(crate) fn escape_into(bytes: &[u8], out: &mut Vec<u8>) {
    let mut start = 0;
    for (i, &b) in bytes.iter().enumerate() {
        let escape: &[u8] = match b {
            b'"' => b"\\\"",
            b'\\' => b"\\\\",
            0x08 => b"\\b",
            0x0C => b"\\f",
            b'\n' => b"\\n",
            b'\r' => b"\\r",
            b'\t' => b"\\t",
            0x00..=0x1F => b"",
            _ => continue,
        };
        out.extend_from_slice(&bytes[start..i]);
        if escape.is_empty() {
            const HEX: &[u8; 16] = b"0123456789abcdef";
            out.extend_from_slice(b"\\u00");
            out.push(HEX[(b >> 4) as usize]);
            out.push(HEX[(b & 0xF) as usize]);
        } else {
            out.extend_from_slice(escape);
        }
        start = i + 1;
    }
    out.extend_from_slice(&bytes[start..]);
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
}
