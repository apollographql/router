//! Vectorized string-content scanning.
//!
//! This is the only module in the crate with architecture-specific paths,
//! and one of the crate's two `unsafe` surfaces (the other is the
//! trusted-UTF-8 view in `utf8`). It classifies string content
//! bytes — closing quote, escape backslash, or unescaped control character —
//! one hardware vector at a time. On aarch64 it uses NEON (baseline for the
//! architecture, so there is no runtime feature detection); everywhere else
//! it falls back to the portable scanner built on `memchr`. Miri cannot
//! interpret vector intrinsics, so Miri runs use the portable scanner too.

/// First special byte in string content, as an offset from the scan start.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum StringScan {
    /// A `"` ending the string.
    Quote(usize),
    /// A `\` starting an escape sequence.
    Escape(usize),
    /// An unescaped control character (below 0x20), which is invalid.
    Control(usize),
    /// No special byte in the input.
    End,
}

/// Finds the first quote, backslash, or control character in `bytes`.
#[inline]
pub(crate) fn scan_string_content(bytes: &[u8]) -> StringScan {
    #[cfg(all(target_arch = "aarch64", not(miri)))]
    {
        neon::scan(bytes)
    }
    #[cfg(any(not(target_arch = "aarch64"), miri))]
    {
        fallback::scan(bytes)
    }
}

#[inline]
fn classify(byte: u8, offset: usize) -> StringScan {
    match byte {
        b'"' => StringScan::Quote(offset),
        b'\\' => StringScan::Escape(offset),
        _ => StringScan::Control(offset),
    }
}

/// Scalar tail for the vector path, and the reference the tests cross-check
/// against.
#[cfg(any(all(target_arch = "aarch64", not(miri)), test))]
#[inline]
fn scan_tail(bytes: &[u8], mut offset: usize) -> StringScan {
    while offset < bytes.len() {
        let b = bytes[offset];
        if b == b'"' || b == b'\\' || b < 0x20 {
            return classify(b, offset);
        }
        offset += 1;
    }
    StringScan::End
}

#[cfg(all(target_arch = "aarch64", not(miri)))]
mod neon {
    use std::arch::aarch64::{
        uint8x16_t, vceqq_u8, vcltq_u8, vdupq_n_u8, vget_lane_u64, vld1q_u8, vmaxvq_u8, vorrq_u8,
        vreinterpret_u64_u8, vreinterpretq_u16_u8, vshrn_n_u16,
    };

    use super::{StringScan, classify, scan_tail};

    /// Lanes flagged 0xFF where the byte is `"`, `\`, or below 0x20.
    ///
    /// # Safety
    /// `ptr` must be readable for 16 bytes. NEON is baseline on aarch64.
    #[inline]
    unsafe fn special_lanes(ptr: *const u8) -> uint8x16_t {
        // SAFETY: caller guarantees 16 readable bytes; vld1q_u8 has no
        // alignment requirement.
        let block = unsafe { vld1q_u8(ptr) };
        unsafe {
            let quote = vceqq_u8(block, vdupq_n_u8(b'"'));
            let escape = vceqq_u8(block, vdupq_n_u8(b'\\'));
            let control = vcltq_u8(block, vdupq_n_u8(0x20));
            vorrq_u8(vorrq_u8(quote, escape), control)
        }
    }

    /// Packs a lane mask (0x00/0xFF per lane) into a nibble-per-lane `u64`,
    /// so `trailing_zeros() / 4` is the first flagged lane index.
    ///
    /// # Safety
    /// NEON is baseline on aarch64.
    #[inline]
    unsafe fn nibble_mask(lanes: uint8x16_t) -> u64 {
        // SAFETY: pure register arithmetic on the given vector.
        unsafe {
            let narrowed = vshrn_n_u16::<4>(vreinterpretq_u16_u8(lanes));
            vget_lane_u64::<0>(vreinterpret_u64_u8(narrowed))
        }
    }

    #[inline]
    pub(super) fn scan(bytes: &[u8]) -> StringScan {
        let len = bytes.len();
        let ptr = bytes.as_ptr();

        // Fast path: most strings (keys, ids) end within the first block.
        if len >= 16 {
            // SAFETY: at least 16 readable bytes.
            let mask = unsafe { nibble_mask(special_lanes(ptr)) };
            if mask != 0 {
                let hit = (mask.trailing_zeros() / 4) as usize;
                return classify(bytes[hit], hit);
            }
        } else {
            return scan_tail(bytes, 0);
        }
        let mut offset = 16;

        // Bulk loop: 64 bytes per iteration with one combined check, for
        // long clean strings.
        while offset + 64 <= len {
            // SAFETY: offset + 64 <= len, so all four loads are in bounds.
            let (a, b, c, d) = unsafe {
                (
                    special_lanes(ptr.add(offset)),
                    special_lanes(ptr.add(offset + 16)),
                    special_lanes(ptr.add(offset + 32)),
                    special_lanes(ptr.add(offset + 48)),
                )
            };
            // SAFETY: register arithmetic only.
            let any = unsafe { vmaxvq_u8(vorrq_u8(vorrq_u8(a, b), vorrq_u8(c, d))) };
            if any != 0 {
                for (i, lanes) in [a, b, c, d].into_iter().enumerate() {
                    // SAFETY: register arithmetic only.
                    let mask = unsafe { nibble_mask(lanes) };
                    if mask != 0 {
                        let hit = offset + i * 16 + (mask.trailing_zeros() / 4) as usize;
                        return classify(bytes[hit], hit);
                    }
                }
                unreachable!("a lane was flagged in the combined check");
            }
            offset += 64;
        }

        // One 16-byte block at a time — this is the fast path for the short
        // strings (keys, ids) that dominate entity traffic.
        while offset + 16 <= len {
            // SAFETY: offset + 16 <= len, so the load is in bounds.
            let mask = unsafe { nibble_mask(special_lanes(ptr.add(offset))) };
            if mask != 0 {
                let hit = offset + (mask.trailing_zeros() / 4) as usize;
                return classify(bytes[hit], hit);
            }
            offset += 16;
        }

        scan_tail(bytes, offset)
    }
}

#[cfg(any(not(target_arch = "aarch64"), miri))]
mod fallback {
    use super::{StringScan, classify};

    /// Portable scan: `memchr2` finds the quote/backslash frontier and the
    /// skipped segment gets one vectorizable control-character sweep.
    pub(super) fn scan(bytes: &[u8]) -> StringScan {
        let frontier = memchr::memchr2(b'"', b'\\', bytes).unwrap_or(bytes.len());
        if let Some(ctl) = bytes[..frontier].iter().position(|&b| b < 0x20) {
            return StringScan::Control(ctl);
        }
        if frontier == bytes.len() {
            return StringScan::End;
        }
        classify(bytes[frontier], frontier)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reference implementation used to cross-check the vector paths.
    fn reference(bytes: &[u8]) -> StringScan {
        scan_tail(bytes, 0)
    }

    /// Every special byte, at every offset across several vector blocks, in
    /// buffers of many lengths — covers lane positions, block boundaries,
    /// and the scalar tail.
    #[test]
    fn special_bytes_found_at_every_lane_position() {
        for &special in &[b'"', b'\\', 0x00, 0x1F, b'\n'] {
            for len in [0usize, 1, 5, 15, 16, 17, 31, 33, 63, 64, 65, 130] {
                for pos in 0..len {
                    let mut bytes = vec![b'x'; len];
                    bytes[pos] = special;
                    assert_eq!(
                        scan_string_content(&bytes),
                        reference(&bytes),
                        "special {special:#x} at {pos} in len {len}"
                    );
                    assert_eq!(scan_string_content(&bytes), classify(special, pos));
                }
            }
        }
    }

    /// The first of several special bytes wins, wherever the runner-up sits.
    #[test]
    fn earliest_special_byte_wins() {
        for first in 0..40 {
            for second in (first + 1)..48 {
                let mut bytes = vec![b'a'; 80];
                bytes[first] = b'\\';
                bytes[second] = b'"';
                assert_eq!(scan_string_content(&bytes), StringScan::Escape(first));
            }
        }
    }

    #[test]
    fn clean_content_reports_end() {
        for len in 0..200 {
            let bytes = vec![b'm'; len];
            assert_eq!(scan_string_content(&bytes), StringScan::End, "len {len}");
        }
    }

    /// Deterministic pseudo-random cross-check against the scalar reference.
    #[test]
    fn agrees_with_reference_on_random_inputs() {
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for _ in 0..2000 {
            let len = (next() % 150) as usize;
            let bytes: Vec<u8> = (0..len).map(|_| (next() % 256) as u8).collect();
            assert_eq!(scan_string_content(&bytes), reference(&bytes), "{bytes:?}");
        }
    }

    /// Multi-byte UTF-8 (high bytes) is never misclassified.
    #[test]
    fn high_bytes_pass_through() {
        let text = "héllo中文😀".repeat(20);
        let mut bytes = text.into_bytes();
        assert_eq!(scan_string_content(&bytes), StringScan::End);
        let len = bytes.len();
        bytes[len - 3] = b'"';
        assert_eq!(scan_string_content(&bytes), StringScan::Quote(len - 3));
    }
}
