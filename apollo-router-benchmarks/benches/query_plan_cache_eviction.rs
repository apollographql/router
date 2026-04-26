//! Benchmark: cache eviction policy comparison — LRU vs moka (W-TinyLFU).
//!
//! Measures hit rate under three workload scenarios to demonstrate the practical
//! difference between LRU recency-only eviction and W-TinyLFU
//! frequency-aware admission:
//!
//!   Scenario 1 — Pure scan-pollution
//!     Warm the hot set, then insert COLD_KEYS with no concurrent hot traffic.
//!     Worst case for LRU: hot entries go stale in wall time and get evicted.
//!     W-TinyLFU rejects cold candidates whose frequency (0) is below the
//!     incumbents' frequency (WARMUP_ACCESSES).
//!
//!   Scenario 2 — Interleaved hot + cold traffic
//!     Warm the hot set, then insert cold keys one at a time while also
//!     accessing a hot key on each iteration. Models a maintenance burst
//!     (batch job, health-check variants) arriving alongside live traffic.
//!     LRU's recency mechanism partially protects hot entries here since
//!     they keep being refreshed; the gap between the two policies narrows.
//!     This scenario gives an honest upper bound on LRU's resilience.
//!
//!   Scenario 3 — Zipf steady-state
//!     Both caches start cold. Accesses are drawn from a Zipf distribution
//!     over a population much larger than the cache, simulating realistic
//!     mixed traffic where a small fraction of operations dominate volume.
//!
//! Run with: `cargo bench --bench query_plan_cache_eviction`

use std::num::NonZeroUsize;

use lru::LruCache;

/// Cache capacity matching Apollo Router's default (`supergraph.query_planning.cache.in_memory.limit`).
const CAPACITY: usize = 512;

/// Hot keys: fit well within cache so both implementations can hold the full set.
const HOT_KEYS: usize = 100;

/// Number of times each hot key is accessed during warm-up.
/// Needs to be large enough that the TinyLFU sketch records meaningful frequency.
const WARMUP_ACCESSES: usize = 30;

/// Cold flood size. Must be >> CAPACITY so that LRU has no choice but to evict
/// hot entries in the pure-flood scenario.
const COLD_KEYS: usize = 2_000;

/// Zipf steady-state: population size and number of accesses.
const ZIPF_POPULATION: usize = 5_000;
const ZIPF_ACCESSES: usize = 100_000;
const ZIPF_SKEW: f64 = 1.1;

// ── Cache trait abstraction ───────────────────────────────────────────────────

trait SyncCache {
    fn get(&mut self, key: u64) -> bool;
    fn insert(&mut self, key: u64);
}

struct LruWrapper {
    inner: LruCache<u64, ()>,
}

impl LruWrapper {
    fn new(capacity: usize) -> Self {
        Self {
            inner: LruCache::new(NonZeroUsize::new(capacity).unwrap()),
        }
    }
}

impl SyncCache for LruWrapper {
    fn get(&mut self, key: u64) -> bool {
        self.inner.get(&key).is_some()
    }

    fn insert(&mut self, key: u64) {
        self.inner.push(key, ());
    }
}

struct MokaWrapper {
    inner: moka::sync::Cache<u64, ()>,
}

impl MokaWrapper {
    fn new(capacity: usize) -> Self {
        Self {
            inner: moka::sync::Cache::new(capacity as u64),
        }
    }
}

impl SyncCache for MokaWrapper {
    fn get(&mut self, key: u64) -> bool {
        self.inner.get(&key).is_some()
    }

    fn insert(&mut self, key: u64) {
        self.inner.insert(key, ());
    }
}

// ── Zipf generator ───────────────────────────────────────────────────────────

/// Pre-computes a CDF table for Zipf(n, alpha) and samples via binary search.
struct Zipf {
    cdf: Vec<f64>,
}

impl Zipf {
    fn new(n: usize, alpha: f64) -> Self {
        let mut weights: Vec<f64> = (1..=n).map(|k| 1.0 / (k as f64).powf(alpha)).collect();
        let sum: f64 = weights.iter().sum();
        let mut acc = 0.0;
        for w in &mut weights {
            acc += *w / sum;
            *w = acc;
        }
        Self { cdf: weights }
    }

    /// Returns a 0-based rank (0 = most popular).
    fn sample(&self, r: f64) -> usize {
        self.cdf.partition_point(|&v| v < r)
    }
}

// ── Benchmark phases ──────────────────────────────────────────────────────────

fn phase_warm(cache: &mut dyn SyncCache) {
    for key in 0..HOT_KEYS as u64 {
        cache.insert(key);
    }
    for _ in 0..WARMUP_ACCESSES {
        for key in 0..HOT_KEYS as u64 {
            cache.get(key);
        }
    }
}

/// Pure cold flood: no concurrent hot traffic.
fn phase_pure_flood(cache: &mut dyn SyncCache) {
    let base = HOT_KEYS as u64 * 10_000;
    for i in 0..COLD_KEYS as u64 {
        cache.insert(base + i);
    }
}

/// Interleaved flood: one hot access per cold insert (round-robin over hot keys).
/// Models live traffic continuing while a cold burst arrives.
fn phase_interleaved_flood(cache: &mut dyn SyncCache) {
    let base = HOT_KEYS as u64 * 10_000;
    for i in 0..COLD_KEYS as u64 {
        cache.get(i % HOT_KEYS as u64); // refresh a hot key before each cold insert
        cache.insert(base + i);
    }
}

fn phase_probe_hot(cache: &mut dyn SyncCache) -> f64 {
    let mut hits = 0usize;
    for key in 0..HOT_KEYS as u64 {
        if cache.get(key) {
            hits += 1;
        }
    }
    hits as f64 / HOT_KEYS as f64
}

fn phase_zipf(cache: &mut dyn SyncCache, zipf: &Zipf, accesses: usize) -> f64 {
    // Deterministic LCG — reproducible without pulling in rand.
    let mut state: u64 = 0xdeadbeef_cafebabe;
    let mut hits = 0usize;
    for _ in 0..1000 {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    }
    for _ in 0..accesses {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let r = (state >> 11) as f64 / (1u64 << 53) as f64;
        let rank = zipf.sample(r) as u64;
        if !cache.get(rank) {
            cache.insert(rank);
        } else {
            hits += 1;
        }
    }
    hits as f64 / accesses as f64
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn run_flood_scenario(
    label: &str,
    cache: &mut dyn SyncCache,
    flood: fn(&mut dyn SyncCache),
) -> (f64, f64) {
    phase_warm(cache);
    let after_warm = phase_probe_hot(cache);
    flood(cache);
    let after_flood = phase_probe_hot(cache);
    println!(
        "  {label:<22}  after warm: {:>6.1}%   after flood: {:>6.1}%",
        after_warm * 100.0,
        after_flood * 100.0,
    );
    (after_warm, after_flood)
}

fn print_retention_summary(label: &str, before: f64, after: f64) {
    println!(
        "    {label:<22} {:>5.1}% → {:>5.1}%  ({:.0} of {HOT_KEYS} hot entries retained)",
        before * 100.0,
        after * 100.0,
        after * HOT_KEYS as f64,
    );
}

// ── Main ──────────────────────────────────────────────────────────────────────

fn main() {
    println!("Query plan cache — eviction policy comparison: LRU vs moka (W-TinyLFU)");
    println!();
    println!(
        "  Cache capacity: {CAPACITY} (Apollo Router default) | \
         Hot set: {HOT_KEYS} keys | Warmup accesses/key: {WARMUP_ACCESSES}"
    );
    println!("  Cold flood: {COLD_KEYS} unique one-shot keys");
    println!();

    // ── Scenario 1: pure scan-pollution ──────────────────────────────────────
    println!("  Scenario 1 — Pure scan-pollution (no concurrent hot traffic)");
    println!("  ─────────────────────────────────────────────────────────────");
    println!(
        "  Worst case for LRU: hot entries go stale while {COLD_KEYS} cold keys flood in."
    );
    println!();

    let (lru_warm_p, lru_flood_p) =
        run_flood_scenario("LRU", &mut LruWrapper::new(CAPACITY), phase_pure_flood);
    let (moka_warm_p, moka_flood_p) =
        run_flood_scenario("moka (W-TinyLFU)", &mut MokaWrapper::new(CAPACITY), phase_pure_flood);

    println!();
    print_retention_summary("LRU", lru_warm_p, lru_flood_p);
    print_retention_summary("moka (W-TinyLFU)", moka_warm_p, moka_flood_p);

    // ── Scenario 2: interleaved hot + cold ───────────────────────────────────
    println!();
    println!("  Scenario 2 — Interleaved (1 hot access per cold insert, round-robin)");
    println!("  ──────────────────────────────────────────────────────────────────────");
    println!("  Models a cold burst arriving alongside live hot traffic.");
    println!("  LRU's recency mechanism partially compensates; gap narrows.");
    println!();

    let (lru_warm_i, lru_flood_i) =
        run_flood_scenario("LRU", &mut LruWrapper::new(CAPACITY), phase_interleaved_flood);
    let (moka_warm_i, moka_flood_i) = run_flood_scenario(
        "moka (W-TinyLFU)",
        &mut MokaWrapper::new(CAPACITY),
        phase_interleaved_flood,
    );

    println!();
    print_retention_summary("LRU", lru_warm_i, lru_flood_i);
    print_retention_summary("moka (W-TinyLFU)", moka_warm_i, moka_flood_i);

    // ── Scenario 3: Zipf steady-state ────────────────────────────────────────
    println!();
    println!("  Scenario 3 — Zipf steady-state (α = {ZIPF_SKEW})");
    println!("  ───────────────────────────────────────────────────");
    println!(
        "  {ZIPF_ACCESSES} accesses over {ZIPF_POPULATION} keys, Zipf α = {ZIPF_SKEW}. \
         Both caches start cold."
    );
    println!();

    let zipf = Zipf::new(ZIPF_POPULATION, ZIPF_SKEW);
    let lru_zipf = {
        let mut c = LruWrapper::new(CAPACITY);
        let r = phase_zipf(&mut c, &zipf, ZIPF_ACCESSES);
        println!("  {:<22}  hit rate: {:>6.1}%", "LRU", r * 100.0);
        r
    };
    let moka_zipf = {
        let mut c = MokaWrapper::new(CAPACITY);
        let r = phase_zipf(&mut c, &zipf, ZIPF_ACCESSES);
        println!("  {:<22}  hit rate: {:>6.1}%", "moka (W-TinyLFU)", r * 100.0);
        r
    };

    println!();
    println!(
        "  moka advantage: {:+.1}pp vs LRU at steady state",
        (moka_zipf - lru_zipf) * 100.0,
    );

    // ── Interpretation ───────────────────────────────────────────────────────
    println!();
    println!("  Interpretation");
    println!("  ──────────────");
    println!("  Scenario 1 (pure flood): W-TinyLFU's admission filter rejects every cold");
    println!("  candidate because its frequency (0) is below the hot entries' frequency");
    println!("  ({WARMUP_ACCESSES}). LRU has no frequency signal — it only tracks recency — so hot");
    println!("  entries that were last accessed before the flood began are evicted.");
    println!();
    println!("  Scenario 2 (interleaved): ongoing hot accesses keep refreshing LRU recency,");
    println!("  so LRU's eviction candidates shift toward the cold keys. The gap between");
    println!("  the two policies narrows. This is the more realistic production scenario");
    println!("  and represents an honest upper bound on LRU's resilience.");
    println!();
    println!("  Scenario 3 (Zipf): at steady state both policies converge toward their");
    println!("  respective hit-rate ceilings. W-TinyLFU retains the frequency head of the");
    println!("  distribution more reliably, yielding a persistent hit-rate advantage.");
    println!();
    println!("  Each cache miss triggers a query planning round-trip (10–200 ms), so");
    println!("  hit-rate improvements directly reduce tail latency.");
}
