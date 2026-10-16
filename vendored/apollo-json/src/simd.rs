//! Vectorized string-content scanning.
//!
//! This is the only module in the crate with architecture-specific paths,
//! and one of the crate's two `unsafe` surfaces (the other is the
//! trusted-UTF-8 view in `utf8`). It classifies string content
//! bytes — closing quote, escape backslash, or unescaped control character —
//! one hardware vector at a time:
//!
//! - aarch64 uses NEON, which is baseline for the architecture, so there is
//!   no runtime feature detection.
//! - x86_64 uses SSE2, which is baseline, and switches to AVX2 for strings
//!   that run past the first 16 bytes when the CPU reports AVX2 at runtime.
//!   Detection runs once per process; see [`string_scanner`].
//! - Everywhere else, and under Miri (which cannot interpret vector
//!   intrinsics), the portable scanner built on `memchr` is used. It is also
//!   the reference the vector paths are tested against.

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
    #[cfg(all(target_arch = "x86_64", not(miri)))]
    {
        x86::scan(bytes)
    }
    #[cfg(any(not(any(target_arch = "aarch64", target_arch = "x86_64")), miri))]
    {
        fallback::scan(bytes)
    }
}

/// Name of the string scanner this process uses: `"neon"`, `"avx2"`,
/// `"sse2"` or `"portable"`.
///
/// On x86_64 the scanner is chosen on first use: AVX2 when the CPU supports
/// it, SSE2 otherwise. The `APOLLO_JSON_SCANNER` environment variable
/// (`portable`, `sse2` or `avx2`), read at that point, overrides the choice
/// for performance comparisons; a request for AVX2 on a CPU without it is
/// ignored. Other architectures have a single scanner and ignore the
/// variable.
pub fn string_scanner() -> &'static str {
    #[cfg(all(target_arch = "aarch64", not(miri)))]
    {
        "neon"
    }
    #[cfg(all(target_arch = "x86_64", not(miri)))]
    {
        x86::Scanner::active().name()
    }
    #[cfg(any(not(any(target_arch = "aarch64", target_arch = "x86_64")), miri))]
    {
        "portable"
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

/// Scalar tail for the vector paths, and the reference the tests cross-check
/// against.
#[cfg(any(
    all(any(target_arch = "aarch64", target_arch = "x86_64"), not(miri)),
    test
))]
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

#[cfg(all(target_arch = "x86_64", not(miri)))]
mod x86 {
    use std::sync::atomic::AtomicU8;
    use std::sync::atomic::Ordering::Relaxed;

    use super::{StringScan, classify, fallback, scan_tail};

    /// The x86_64 scanners. `Portable` is only ever chosen by the
    /// `APOLLO_JSON_SCANNER` override, since SSE2 is always available.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub(super) enum Scanner {
        Portable = 1,
        Sse2 = 2,
        Avx2 = 3,
    }

    /// The selected [`Scanner`] as its discriminant; 0 until first use.
    static ACTIVE: AtomicU8 = AtomicU8::new(0);

    impl Scanner {
        #[inline]
        pub(super) fn active() -> Self {
            match ACTIVE.load(Relaxed) {
                1 => Self::Portable,
                2 => Self::Sse2,
                3 => Self::Avx2,
                _ => Self::select(),
            }
        }

        /// Detects the CPU's best scanner and applies the override. Racing
        /// first calls all store the same value.
        #[cold]
        fn select() -> Self {
            let best = if std::arch::is_x86_feature_detected!("avx2") {
                Self::Avx2
            } else {
                Self::Sse2
            };
            let chosen = match std::env::var("APOLLO_JSON_SCANNER").as_deref() {
                Ok("portable") => Self::Portable,
                Ok("sse2") => Self::Sse2,
                _ => best,
            };
            ACTIVE.store(chosen as u8, Relaxed);
            chosen
        }

        pub(super) fn name(self) -> &'static str {
            match self {
                Self::Portable => "portable",
                Self::Sse2 => "sse2",
                Self::Avx2 => "avx2",
            }
        }
    }

    #[inline]
    pub(super) fn scan(bytes: &[u8]) -> StringScan {
        scan_with(Scanner::active(), bytes)
    }

    /// Scans with `scanner`, which must be [`Scanner::active`] or a scanner
    /// the CPU supports.
    #[inline]
    pub(super) fn scan_with(scanner: Scanner, bytes: &[u8]) -> StringScan {
        if scanner == Scanner::Portable {
            return fallback::scan(bytes);
        }
        // Fast path: most strings (keys, ids) end within the first block.
        // SSE2 checks it inline; the AVX2 loop is a separate, non-inlined
        // function, so it only pays for strings that run longer.
        if bytes.len() < 16 {
            return scan_tail(bytes, 0);
        }
        // SAFETY: at least 16 readable bytes.
        let mask = unsafe { sse2::mask16(bytes.as_ptr()) };
        if mask != 0 {
            let hit = mask.trailing_zeros() as usize;
            return classify(bytes[hit], hit);
        }
        match scanner {
            // SAFETY: Avx2 is only selected when the CPU reports AVX2.
            Scanner::Avx2 => unsafe { avx2::scan_from(bytes, 16) },
            _ => sse2::scan_from(bytes, 16),
        }
    }

    pub(super) mod sse2 {
        use std::arch::x86_64::{
            __m128i, _mm_cmpeq_epi8, _mm_loadu_si128, _mm_max_epu8, _mm_movemask_epi8,
            _mm_or_si128, _mm_set1_epi8,
        };

        use super::super::{StringScan, classify, scan_tail};

        /// One bit per byte, set where the byte is `"`, `\`, or below 0x20.
        ///
        /// # Safety
        /// `ptr` must be readable for 16 bytes. SSE2 is baseline on x86_64.
        #[inline]
        pub(crate) unsafe fn mask16(ptr: *const u8) -> u32 {
            // SAFETY: caller guarantees 16 readable bytes; the load is
            // unaligned.
            let block = unsafe { _mm_loadu_si128(ptr.cast::<__m128i>()) };
            // SAFETY: register arithmetic only, on baseline SSE2.
            unsafe {
                let quote = _mm_cmpeq_epi8(block, _mm_set1_epi8(b'"' as i8));
                let escape = _mm_cmpeq_epi8(block, _mm_set1_epi8(b'\\' as i8));
                // SSE2 compares are signed; max(b, 0x1F) == 0x1F is unsigned b <= 0x1F.
                let ceiling = _mm_set1_epi8(0x1F);
                let control = _mm_cmpeq_epi8(_mm_max_epu8(block, ceiling), ceiling);
                _mm_movemask_epi8(_mm_or_si128(_mm_or_si128(quote, escape), control)) as u32
            }
        }

        /// Scans `bytes[offset..]`, reporting offsets from the start of `bytes`.
        #[inline]
        pub(crate) fn scan_from(bytes: &[u8], mut offset: usize) -> StringScan {
            let len = bytes.len();
            let ptr = bytes.as_ptr();

            // Bulk loop: 64 bytes per iteration, packed into one mask.
            while offset + 64 <= len {
                // SAFETY: offset + 64 <= len, so all four loads are in bounds.
                let mask = unsafe {
                    u64::from(mask16(ptr.add(offset)))
                        | u64::from(mask16(ptr.add(offset + 16))) << 16
                        | u64::from(mask16(ptr.add(offset + 32))) << 32
                        | u64::from(mask16(ptr.add(offset + 48))) << 48
                };
                if mask != 0 {
                    let hit = offset + mask.trailing_zeros() as usize;
                    return classify(bytes[hit], hit);
                }
                offset += 64;
            }

            while offset + 16 <= len {
                // SAFETY: offset + 16 <= len, so the load is in bounds.
                let mask = unsafe { mask16(ptr.add(offset)) };
                if mask != 0 {
                    let hit = offset + mask.trailing_zeros() as usize;
                    return classify(bytes[hit], hit);
                }
                offset += 16;
            }

            scan_tail(bytes, offset)
        }
    }

    pub(super) mod avx2 {
        use std::arch::x86_64::{
            __m256i, _mm256_cmpeq_epi8, _mm256_loadu_si256, _mm256_max_epu8, _mm256_movemask_epi8,
            _mm256_or_si256, _mm256_set1_epi8,
        };

        use super::super::{StringScan, classify};
        use super::sse2;

        /// One bit per byte, set where the byte is `"`, `\`, or below 0x20.
        ///
        /// # Safety
        /// `ptr` must be readable for 32 bytes, and the CPU must support AVX2.
        #[inline]
        #[target_feature(enable = "avx2")]
        unsafe fn mask32(ptr: *const u8) -> u32 {
            // SAFETY: caller guarantees 32 readable bytes; the load is
            // unaligned.
            let block = unsafe { _mm256_loadu_si256(ptr.cast::<__m256i>()) };
            let quote = _mm256_cmpeq_epi8(block, _mm256_set1_epi8(b'"' as i8));
            let escape = _mm256_cmpeq_epi8(block, _mm256_set1_epi8(b'\\' as i8));
            // Signed compares again; max(b, 0x1F) == 0x1F is unsigned b <= 0x1F.
            let ceiling = _mm256_set1_epi8(0x1F);
            let control = _mm256_cmpeq_epi8(_mm256_max_epu8(block, ceiling), ceiling);
            _mm256_movemask_epi8(_mm256_or_si256(_mm256_or_si256(quote, escape), control)) as u32
        }

        /// Scans `bytes[offset..]`, reporting offsets from the start of `bytes`.
        ///
        /// # Safety
        /// The CPU must support AVX2.
        #[target_feature(enable = "avx2")]
        pub(crate) unsafe fn scan_from(bytes: &[u8], mut offset: usize) -> StringScan {
            let len = bytes.len();
            let ptr = bytes.as_ptr();

            // Bulk loop: 64 bytes per iteration, packed into one mask.
            while offset + 64 <= len {
                // SAFETY: offset + 64 <= len, so both loads are in bounds.
                let mask = unsafe {
                    u64::from(mask32(ptr.add(offset)))
                        | u64::from(mask32(ptr.add(offset + 32))) << 32
                };
                if mask != 0 {
                    let hit = offset + mask.trailing_zeros() as usize;
                    return classify(bytes[hit], hit);
                }
                offset += 64;
            }

            if offset + 32 <= len {
                // SAFETY: offset + 32 <= len, so the load is in bounds.
                let mask = unsafe { mask32(ptr.add(offset)) };
                if mask != 0 {
                    let hit = offset + mask.trailing_zeros() as usize;
                    return classify(bytes[hit], hit);
                }
                offset += 32;
            }

            // Fewer than 32 bytes left: SSE2 blocks, then the scalar tail.
            sse2::scan_from(bytes, offset)
        }
    }
}

#[cfg(any(not(target_arch = "aarch64"), miri, test))]
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

    type Scan = fn(&[u8]) -> StringScan;

    /// Every scanner this test process can run, by name. The dispatcher is
    /// always included, so the selected scanner is covered as well.
    #[cfg_attr(not(target_arch = "x86_64"), allow(unused_mut))]
    fn scanners() -> Vec<(&'static str, Scan)> {
        let mut all: Vec<(&'static str, Scan)> = vec![
            ("dispatch", scan_string_content),
            ("portable", fallback::scan),
        ];
        #[cfg(all(target_arch = "x86_64", not(miri)))]
        {
            use x86::{Scanner, scan_with};
            all.push(("sse2", |b| scan_with(Scanner::Sse2, b)));
            all.push(("sse2 bulk", |b| x86::sse2::scan_from(b, 0)));
            if std::arch::is_x86_feature_detected!("avx2") {
                all.push(("avx2", |b| scan_with(Scanner::Avx2, b)));
                // SAFETY: AVX2 was just detected.
                all.push(("avx2 bulk", |b| unsafe { x86::avx2::scan_from(b, 0) }));
            }
        }
        all
    }

    /// Checks every scanner against the reference on `bytes`, and on copies
    /// of `bytes` shifted so loads start at other 16- and 32-byte alignments.
    fn check_all(bytes: &[u8], context: &str) {
        let expected = reference(bytes);
        for (name, scan) in scanners() {
            assert_eq!(scan(bytes), expected, "{name}: {context}");
        }
        let mut padded = vec![0u8; bytes.len() + 32];
        for shift in [1, 3, 8, 15, 16, 17, 31] {
            padded[shift..shift + bytes.len()].copy_from_slice(bytes);
            let shifted = &padded[shift..shift + bytes.len()];
            for (name, scan) in scanners() {
                assert_eq!(
                    scan(shifted),
                    expected,
                    "{name}: {context}, shifted {shift}"
                );
            }
        }
    }

    /// Lengths around every block size the scanners use (16, 32, 64) and
    /// the first-block fast path.
    const LENGTHS: &[usize] = &[
        0, 1, 2, 5, 15, 16, 17, 31, 32, 33, 47, 48, 63, 64, 65, 79, 80, 95, 96, 97, 127, 128, 129,
        143, 144, 191, 192, 193, 255, 256, 257,
    ];

    /// Every special byte, at every offset, in buffers of many lengths —
    /// covers lane positions, block boundaries, and the scalar tail.
    #[test]
    fn special_bytes_found_at_every_lane_position() {
        for &special in &[b'"', b'\\', 0x00, 0x1F, b'\n'] {
            for &len in LENGTHS {
                for pos in 0..len {
                    let mut bytes = vec![b'x'; len];
                    bytes[pos] = special;
                    check_all(
                        &bytes,
                        &format!("special {special:#x} at {pos} in len {len}"),
                    );
                    assert_eq!(scan_string_content(&bytes), classify(special, pos));
                }
            }
        }
    }

    /// Bytes next to the special set — 0x20, 0x7F, 0x80, 0xFF, and the
    /// bytes either side of `"` and `\` — are never flagged, which catches
    /// a signed compare or an off-by-one ceiling.
    #[test]
    fn neighbours_of_special_bytes_pass_through() {
        for &byte in &[
            0x20u8, 0x21, 0x23, 0x5B, 0x5D, 0x7F, 0x80, 0x9F, 0xA0, 0xDC, 0xFF,
        ] {
            for &len in LENGTHS {
                check_all(&vec![byte; len], &format!("byte {byte:#x} in len {len}"));
            }
        }
    }

    /// The first of several special bytes wins, wherever the runner-up sits,
    /// including in a later block.
    #[test]
    fn earliest_special_byte_wins() {
        for first in 0..72 {
            for second in (first + 1)..140 {
                let mut bytes = vec![b'a'; 160];
                bytes[first] = b'\\';
                bytes[second] = if second % 2 == 0 { b'"' } else { 0x01 };
                check_all(&bytes, &format!("first {first}, second {second}"));
                assert_eq!(scan_string_content(&bytes), StringScan::Escape(first));
            }
        }
    }

    #[test]
    fn clean_content_reports_end() {
        for len in 0..300 {
            let bytes = vec![b'm'; len];
            check_all(&bytes, &format!("len {len}"));
            assert_eq!(scan_string_content(&bytes), StringScan::End, "len {len}");
        }
    }

    /// Deterministic pseudo-random cross-check against the scalar reference,
    /// with special bytes sparse enough that many inputs reach the bulk loops.
    #[test]
    fn agrees_with_reference_on_random_inputs() {
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for round in 0..3000 {
            let len = (next() % 400) as usize;
            let density = [2u64, 64, 512][round % 3];
            let bytes: Vec<u8> = (0..len)
                .map(|_| {
                    let roll = next();
                    if roll % density == 0 {
                        [b'"', b'\\', 0x00, 0x1F][(roll >> 32) as usize % 4]
                    } else {
                        0x20 + (roll >> 8) as u8 % 0xE0
                    }
                })
                .collect();
            check_all(&bytes, &format!("{bytes:?}"));
        }
    }

    /// Multi-byte UTF-8 (high bytes) is never misclassified, and a special
    /// byte after it is found at its exact offset.
    #[test]
    fn high_bytes_pass_through() {
        let text = "héllo中文😀".repeat(20);
        let mut bytes = text.into_bytes();
        check_all(&bytes, "multibyte only");
        assert_eq!(scan_string_content(&bytes), StringScan::End);
        let len = bytes.len();
        for pos in [len - 3, 17, 33, 65] {
            let mut marked = bytes.clone();
            marked[pos] = b'"';
            check_all(&marked, &format!("multibyte with quote at {pos}"));
        }
        bytes[len - 3] = b'"';
        assert_eq!(scan_string_content(&bytes), StringScan::Quote(len - 3));
    }

    /// The selected scanner follows the CPU and the `APOLLO_JSON_SCANNER`
    /// override, and an AVX2 request on a CPU without AVX2 gets SSE2.
    #[test]
    #[cfg(all(target_arch = "x86_64", not(miri)))]
    fn x86_scanner_follows_cpu_and_override() {
        let avx2 = std::arch::is_x86_feature_detected!("avx2");
        let best = if avx2 { "avx2" } else { "sse2" };
        let expected = match std::env::var("APOLLO_JSON_SCANNER").as_deref() {
            Ok("portable") => "portable",
            Ok("sse2") => "sse2",
            _ => best,
        };
        assert_eq!(string_scanner(), expected);
    }
}
