use anyhow::{Result, bail};
use wit_parser::{Resolve, WorldId, decoding::DecodedWasm};

pub fn decode_component(bytes: &[u8]) -> Result<(Resolve, WorldId)> {
    // wit_component::decode bails when a `package-docs` custom section names a
    // world that doesn't exist in the synthesized component package (common for
    // jco-built components, which embed docs referencing the source WIT world
    // even though the binary itself doesn't carry a fully-named world). Strip
    // it before decoding — it's pure documentation metadata.
    let stripped = strip_top_level_custom_section(bytes, "package-docs");
    let input = stripped.as_deref().unwrap_or(bytes);

    match wit_component::decode(input)? {
        DecodedWasm::Component(resolve, world) => Ok((resolve, world)),
        DecodedWasm::WitPackage(..) => {
            bail!("input is a WIT package, not a component");
        }
    }
}

/// Walk top-level wasm sections, splicing out any custom section with the given
/// name. Returns `Some(new_bytes)` if a match was found and removed, `None` if
/// the input is unchanged. The wasm and component binary formats share section
/// framing: `id: u8` + `size: leb128` + `body[size]`; for custom sections
/// (id=0) the body starts with `name_len: leb128` + `name` + `data`.
fn strip_top_level_custom_section(bytes: &[u8], target: &str) -> Option<Vec<u8>> {
    if bytes.len() < 8 {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len());
    out.extend_from_slice(&bytes[..8]); // magic + version
    let mut i = 8;
    let mut removed = false;

    while i < bytes.len() {
        let section_start = i;
        let id = bytes[i];
        i += 1;
        let Some((size, size_len)) = read_leb128_u32(&bytes[i..]) else {
            return None;
        };
        i += size_len;
        let body_start = i;
        let body_end = body_start.checked_add(size as usize)?;
        if body_end > bytes.len() {
            return None;
        }

        if id == 0 {
            let Some((name_len, name_len_size)) = read_leb128_u32(&bytes[body_start..body_end])
            else {
                return None;
            };
            let name_start = body_start + name_len_size;
            let name_end = name_start.checked_add(name_len as usize)?;
            if name_end > body_end {
                return None;
            }
            let name = std::str::from_utf8(&bytes[name_start..name_end]).ok()?;
            if name == target {
                removed = true;
                i = body_end;
                continue;
            }
        }

        out.extend_from_slice(&bytes[section_start..body_end]);
        i = body_end;
    }

    if removed { Some(out) } else { None }
}

fn read_leb128_u32(buf: &[u8]) -> Option<(u32, usize)> {
    let mut result: u32 = 0;
    let mut shift = 0;
    for (i, &byte) in buf.iter().enumerate().take(5) {
        let low = (byte & 0x7f) as u32;
        result |= low.checked_shl(shift)?;
        if byte & 0x80 == 0 {
            return Some((result, i + 1));
        }
        shift += 7;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leb128_single_byte() {
        assert_eq!(read_leb128_u32(&[0x00]), Some((0, 1)));
        assert_eq!(read_leb128_u32(&[0x7f]), Some((127, 1)));
    }

    #[test]
    fn leb128_multi_byte() {
        // 624485 = 0xe5, 0x8e, 0x26 in LEB128
        assert_eq!(read_leb128_u32(&[0xe5, 0x8e, 0x26]), Some((624485, 3)));
    }

    #[test]
    fn strip_removes_matching_section() {
        // Hand-built minimal component: header + one custom section "foo" with data 0xaa.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"\0asm");
        bytes.extend_from_slice(&[0x0a, 0x00, 0x01, 0x00]); // component version
        // section: id=0, body = [name_len=3, "foo", 0xaa]
        bytes.push(0x00);
        bytes.push(5); // body size: 1 (name_len) + 3 (name) + 1 (data)
        bytes.push(3);
        bytes.extend_from_slice(b"foo");
        bytes.push(0xaa);

        let stripped = strip_top_level_custom_section(&bytes, "foo").expect("should strip");
        assert_eq!(stripped, &bytes[..8]);

        // Targeting a non-matching name returns None.
        assert!(strip_top_level_custom_section(&bytes, "bar").is_none());
    }
}
