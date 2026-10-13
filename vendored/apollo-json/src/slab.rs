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

    /// This run after the store it lives in was appended to another one
    /// whose chunks come first; see [`Slabs::absorb`].
    pub(crate) fn shift_chunk(self, by: u16) -> SlabRef {
        if self.len == 0 {
            return SlabRef::EMPTY;
        }
        SlabRef {
            chunk: self.chunk + by,
            ..self
        }
    }

    /// This run with `len` live elements, for a container that grew into
    /// its room or shrank. A container with none is the empty run, so the
    /// room it leaves is never claimed again.
    pub(crate) fn with_len(self, len: usize) -> SlabRef {
        if len == 0 {
            return SlabRef::EMPTY;
        }
        SlabRef {
            len: u32::try_from(len).expect("slab length within the arena cap"),
            ..self
        }
    }
}

pub(crate) struct Slabs<T> {
    chunks: Vec<Vec<T>>,
    /// The chunk currently being filled. Chunks after it are cleared
    /// leftovers from a recycled store, or chunks absorbed from another
    /// store, and are filled before anything new is allocated.
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

    /// Makes room in `run` for at least `extra` elements past its live
    /// ones and returns where the run lives now, with the same live length.
    /// Room is marked with `vacant`, which `is_vacant` recognises.
    ///
    /// Vacant slots right after a run's live elements are that run's room:
    /// nothing else is ever written there, because storage only grows at a
    /// chunk's end. So a run with enough room already is returned as it is,
    /// and a run that ends the chunk being filled grows where it is. Any other
    /// run moves to the end of storage. With `exact` it gets exactly the room
    /// asked for, for a caller that knows how much it will write; otherwise
    /// it gets room to double, so a container growing one element at a time
    /// moves O(log n) times. The run left behind is dead space in its chunk,
    /// like any replaced slot.
    pub(crate) fn grow(
        &mut self,
        run: SlabRef,
        extra: usize,
        exact: bool,
        vacant: T,
        is_vacant: impl Fn(&T) -> bool,
    ) -> SlabRef {
        let used = run.len();
        if used > 0 {
            let current = run.chunk as usize == self.current;
            let chunk = &mut self.chunks[run.chunk as usize];
            let end = run.start as usize + used;
            let room = chunk[end..]
                .iter()
                .take(extra)
                .take_while(|slot| is_vacant(slot))
                .count();
            if room == extra {
                return run;
            }
            if current && end + room == chunk.len() && chunk.capacity() - end >= extra {
                let grown = if exact { extra } else { extra.max(used.max(4)) };
                chunk.resize(end + grown.min(chunk.capacity() - end), vacant);
                return run;
            }
        }
        let capacity = if exact {
            used + extra
        } else {
            (used * 2).max(used + 4).max(used + extra)
        };
        if !self.fits_in_current(capacity) {
            self.provision(capacity);
        }
        let to = self.current;
        let from = run.chunk as usize;
        let start = self.chunks[to].len();
        if used > 0 {
            let range = run.start as usize..run.start as usize + used;
            if from == to {
                self.chunks[to].extend_from_within(range);
            } else {
                let (source, target) = if from < to {
                    let (low, high) = self.chunks.split_at_mut(to);
                    (&low[from], &mut high[0])
                } else {
                    let (low, high) = self.chunks.split_at_mut(from);
                    (&high[0], &mut low[to])
                };
                target.extend_from_slice(&source[range]);
            }
        }
        self.chunks[to].resize(start + capacity, vacant);
        SlabRef {
            chunk: u16::try_from(to).expect("chunk count within the arena cap"),
            start: u16::try_from(start).expect("in-chunk offsets fit in u16"),
            len: run.len,
        }
    }

    /// Slots written so far, live or vacant.
    #[cfg(test)]
    pub(crate) fn slots(&self) -> usize {
        self.chunks.iter().map(Vec::len).sum()
    }

    /// Number of chunks, which is what a store absorbed into this one shifts
    /// its runs by.
    pub(crate) fn chunk_count(&self) -> u16 {
        u16::try_from(self.chunks.len()).expect("chunk count within the arena cap")
    }

    /// Whether `other`'s chunks can follow this store's within the chunk
    /// index range.
    pub(crate) fn can_absorb(&self, other: &Slabs<T>) -> bool {
        self.chunks.len() + other.chunks.len() <= usize::from(u16::MAX)
    }

    /// Applies `f` to every stored element, live or not.
    pub(crate) fn for_each_mut(&mut self, mut f: impl FnMut(&mut T)) {
        for chunk in &mut self.chunks {
            chunk.iter_mut().for_each(&mut f);
        }
    }

    /// Moves `other`'s chunks after this store's; its runs move by
    /// [`Slabs::chunk_count`] as it was before the call. The moved chunks
    /// are filled after the current one, as recycled ones are.
    pub(crate) fn absorb(&mut self, other: &mut Slabs<T>) {
        self.bytes += other.bytes;
        other.bytes = 0;
        self.chunks.append(&mut other.chunks);
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
        // Reuse the next later chunk with room for the run.
        for next in self.current + 1..self.chunks.len() {
            let chunk = &self.chunks[next];
            if chunk.len() + need <= chunk.capacity() {
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

    /// Appends one element to `run`, the way a builder grows a container.
    fn push(slabs: &mut Slabs<u32>, run: SlabRef, value: u32) -> SlabRef {
        let used = run.len();
        let run = slabs
            .grow(run, 1, false, u32::MAX, |slot| *slot == u32::MAX)
            .with_len(used + 1);
        slabs.get_mut(run)[used] = value;
        run
    }

    #[test]
    fn a_run_ending_the_current_chunk_grows_in_place() {
        let mut slabs: Slabs<u32> = Slabs::new();
        slabs.alloc(&[0; 64]);
        let run = slabs.alloc(&[1, 2, 3]);
        let grown = push(&mut slabs, run, 4);
        assert_eq!((grown.chunk, grown.start), (run.chunk, run.start));
        assert_eq!(slabs.get(grown), &[1, 2, 3, 4]);
        // The growth left room, so the next push stays put even with
        // another run allocated behind it.
        slabs.alloc(&[0]);
        let again = push(&mut slabs, grown, 5);
        assert_eq!((again.chunk, again.start), (run.chunk, run.start));
        assert_eq!(slabs.get(again), &[1, 2, 3, 4, 5]);
    }

    #[test]
    fn growing_runs_interleaved_keep_every_run_intact() {
        // Runs growing in turn cannot all end the chunk, so each move leaves
        // the others untouched and carries its own prefix along.
        let mut slabs: Slabs<u32> = Slabs::new();
        let mut runs: Vec<(SlabRef, Vec<u32>)> = (0..50u32)
            .map(|i| (slabs.alloc(&[i * 1000]), vec![i * 1000]))
            .collect();
        for step in 1..200u32 {
            for (i, (run, expected)) in runs.iter_mut().enumerate() {
                let value = i as u32 * 1000 + step;
                *run = push(&mut slabs, *run, value);
                expected.push(value);
            }
        }
        for (run, expected) in &runs {
            assert_eq!(slabs.get(*run), expected.as_slice());
        }
    }

    #[test]
    fn growth_copies_a_run_a_logarithmic_number_of_times() {
        let mut slabs: Slabs<u32> = Slabs::new();
        let mut run = SlabRef::EMPTY;
        let mut moves = 0;
        for value in 0..10_000u32 {
            // Another run allocated after this one keeps it from simply
            // extending at the chunk's end, so every regrowth is a move.
            let before = (run.chunk, run.start);
            run = push(&mut slabs, run, value);
            if (run.chunk, run.start) != before {
                moves += 1;
                slabs.alloc(&[0]);
            }
        }
        assert!(moves <= 20, "{moves} moves");
        let expected: Vec<u32> = (0..10_000).collect();
        assert_eq!(slabs.get(run), expected.as_slice());
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
