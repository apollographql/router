//! Chunked slab storage for container children, object entries, and owned
//! text.
//!
//! A slab is a contiguous run of `Copy` elements written once and addressed
//! by `(chunk, start, len)` indices, so nodes stay plain data and the whole
//! store frees chunk-at-a-time with no per-element work. Growth follows the
//! arena's bounded policy: the first chunk ramps up to the fixed chunk size,
//! then storage grows by whole fixed-size chunks; a run larger than a chunk
//! gets a dedicated chunk sized exactly to the run.

/// Bytes per full chunk.
const CHUNK_BYTES: usize = 64 * 1024;

/// Reference to a contiguous run of elements in a [`Slabs`] store.
///
/// `start` fits in `u16` by construction: standard chunks hold at most
/// 64 KiB of single-byte elements (offsets up to 65535), and oversized
/// dedicated chunks always hold their single run at offset zero.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SlabRef {
    chunk: u16,
    start: u16,
    len: u32,
}

impl SlabRef {
    pub(crate) const EMPTY: SlabRef = SlabRef {
        chunk: 0,
        start: 0,
        len: 0,
    };

    pub(crate) fn len(&self) -> usize {
        self.len as usize
    }
}

pub(crate) struct Slabs<T> {
    chunks: Vec<Vec<T>>,
    /// The chunk currently being filled. Chunks after it are cleared
    /// leftovers from a recycled store, reused before anything new is
    /// allocated.
    current: usize,
    /// Elements per full chunk.
    chunk_elems: usize,
    /// Bytes of chunk storage (by capacity), maintained incrementally for
    /// the arena size cap.
    bytes: usize,
}

impl<T: Copy> Slabs<T> {
    pub(crate) fn new() -> Self {
        Slabs {
            chunks: Vec::new(),
            current: 0,
            chunk_elems: CHUNK_BYTES / size_of::<T>(),
            bytes: 0,
        }
    }

    pub(crate) fn bytes(&self) -> usize {
        self.bytes
    }

    /// Clears the store for reuse, keeping standard chunks (and their
    /// capacity) and dropping oversized dedicated ones, whose reuse could
    /// place run offsets beyond `SlabRef`'s `u16` range.
    pub(crate) fn reset(&mut self) {
        self.chunks
            .retain(|chunk| chunk.capacity() <= self.chunk_elems);
        for chunk in &mut self.chunks {
            chunk.clear();
        }
        self.current = 0;
        self.bytes = self.chunks.iter().map(Vec::capacity).sum::<usize>() * size_of::<T>();
    }

    /// Copies `items` into slab storage as one contiguous run.
    pub(crate) fn alloc(&mut self, items: &[T]) -> SlabRef {
        if items.is_empty() {
            return SlabRef::EMPTY;
        }
        if !self.fits_in_current(items.len()) {
            self.provision(items.len());
        }
        let chunk_index = self.current;
        let chunk = &mut self.chunks[chunk_index];
        let start = chunk.len();
        chunk.extend_from_slice(items);
        SlabRef {
            chunk: u16::try_from(chunk_index).expect("chunk count within the arena cap"),
            start: u16::try_from(start).expect("in-chunk offsets fit in u16"),
            len: u32::try_from(items.len()).expect("slab length within the arena cap"),
        }
    }

    pub(crate) fn get(&self, slab: SlabRef) -> &[T] {
        if slab.len == 0 {
            return &[];
        }
        let start = slab.start as usize;
        &self.chunks[slab.chunk as usize][start..start + slab.len as usize]
    }

    pub(crate) fn get_mut(&mut self, slab: SlabRef) -> &mut [T] {
        if slab.len == 0 {
            return &mut [];
        }
        let start = slab.start as usize;
        &mut self.chunks[slab.chunk as usize][start..start + slab.len as usize]
    }

    fn fits_in_current(&self, need: usize) -> bool {
        self.chunks
            .get(self.current)
            .is_some_and(|chunk| chunk.len() + need <= chunk.capacity())
    }

    /// Makes room for `need` more elements. Growing a chunk in place is safe
    /// because slabs are addressed by index, not pointer.
    fn provision(&mut self, need: usize) {
        if let Some(chunk) = self.chunks.get_mut(self.current) {
            let target = chunk.len() + need;
            if target <= self.chunk_elems && chunk.capacity() < self.chunk_elems {
                // Ramp the (estimate-sized) chunk toward the fixed size; the
                // transient waste is bounded by one chunk.
                let goal = (chunk.capacity() * 2).max(target).min(self.chunk_elems);
                self.bytes += (goal - chunk.capacity()) * size_of::<T>();
                chunk.reserve_exact(goal - chunk.len());
                return;
            }
        }
        // Reuse the next cleared leftover chunk that can hold the run.
        for next in self.current + 1..self.chunks.len() {
            if need <= self.chunks[next].capacity() {
                self.current = next;
                return;
            }
        }
        let capacity = if need >= self.chunk_elems {
            // Dedicated chunk sized exactly for one oversized run.
            need
        } else if self.chunks.is_empty() {
            // First chunk starts at the caller's first-run size and ramps.
            need.max(8)
        } else {
            self.chunk_elems
        };
        self.bytes += capacity * size_of::<T>();
        self.chunks.push(Vec::with_capacity(capacity));
        self.current = self.chunks.len() - 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_are_contiguous_and_stable_across_growth() {
        let mut slabs: Slabs<u32> = Slabs::new();
        let refs: Vec<(SlabRef, Vec<u32>)> = (0..1000u32)
            .map(|i| {
                let run: Vec<u32> = (0..i % 17).map(|j| i * 100 + j).collect();
                (slabs.alloc(&run), run)
            })
            .collect();
        for (slab, expected) in refs {
            assert_eq!(slabs.get(slab), expected.as_slice());
        }
    }

    #[test]
    fn oversized_runs_get_dedicated_chunks() {
        let mut slabs: Slabs<u64> = Slabs::new();
        let small = slabs.alloc(&[1, 2, 3]);
        let big: Vec<u64> = (0..50_000).collect();
        let slab = slabs.alloc(&big);
        assert_eq!(slabs.get(slab), big.as_slice());
        assert_eq!(slabs.get(small), &[1, 2, 3]);
        let after = slabs.alloc(&[9]);
        assert_eq!(slabs.get(after), &[9]);
    }

    #[test]
    fn bytes_track_capacity_linearly() {
        let mut slabs: Slabs<u8> = Slabs::new();
        slabs.alloc(&[0; 100]);
        assert!(slabs.bytes() >= 100);
        for _ in 0..40 {
            slabs.alloc(&[0; 10_000]);
        }
        // Fixed-size growth: bytes stay proportional to stored data plus at
        // most one chunk of slack.
        assert!(
            slabs.bytes() <= 400_100 + 2 * 64 * 1024,
            "{}",
            slabs.bytes()
        );
    }
}
