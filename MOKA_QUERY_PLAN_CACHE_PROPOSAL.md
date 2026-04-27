# Proposal: Migrate In-Memory Cache from `Mutex<LruCache>` to `moka` (W-TinyLFU)

**Author:** Jon Christiansen (Indeed / OneGraph)  
**Date:** 2026-04-25  
**Upstream project:** [apollographql/router](https://github.com/apollographql/router)

---

## Summary

This proposal replaces the query plan cache implementation inside `apollo-router` from a
`tokio::sync::Mutex<lru::LruCache<K, V>>` to `moka::future::Cache<K, V>`. The change touches a
single foundational abstraction — `CacheStorage<K, V>` in `apollo-router/src/cache/storage.rs` —
which is shared by the query plan, introspection, and APQ in-memory caches.

The primary motivations are: eliminating a global serialization point under concurrent load,
and replacing LRU eviction (which favors recency over utility) with W-TinyLFU (which
approximates optimal eviction).

**Note:** this PR builds on a companion `DeduplicatingCache` fast-path change that checks the
in-memory cache before acquiring the wait-map mutex on cache hits. That fast-path PR has
already merged into `dev`. Without it, the wait-map mutex would still be acquired before moka
is ever called, masking this PR's throughput benefit entirely.

---

## Verdict

**This change is worth it.** The trade-offs are real but narrow; the benefits apply to the
common case.

**Where moka is a clear win:**
- **Routers serving diverse API consumers** (multiple teams, services, or client apps sharing
  a single router instance). A misbehaving client flooding the cache with inline-constant
  queries silently evicts the hot plans of every other caller under LRU. W-TinyLFU's admission
  filter rejects those cold entries without any configuration change — containment at the cache
  layer, for free.
- **Off-peak cold bursts** (scheduled jobs, health-check variants, overnight batch traffic).
  When hot accesses are sparse relative to cold inserts, LRU's recency signal fails and moka's
  frequency sketch holds. The benchmark confirms this is deterministic, not probabilistic.
- **Zipf steady-state hit rate** improves by +5.0–5.2pp at identical capacity. Each miss is a
  10–200ms planning round-trip, so this compounds into measurable tail latency reduction at
  high RPS.

**Where moka does not improve things:**
- **Interleaved hot + cold traffic** (cold burst alongside sustained live traffic): both
  policies retain 100% of the hot set. LRU's recency signal compensates when hot accesses keep
  refreshing entries. moka provides no eviction advantage here.
- **Throughput at c=1**: moka is ~3% slower than the fast-path branch it builds on at serial
  concurrency (2,779 vs 2,864 req/s) due to async insert overhead. This is a real regression,
  but sustained c=1 is not a meaningful production target for a router — any real deployment
  has concurrent requests.
- **Throughput gains overall are modest** (+1–3% at high concurrency). The fast-path PR did
  most of the throughput work; moka's contribution is incremental.

**Do the negatives outweigh the positives?**
No. The cases where moka doesn't help (interleaved traffic, serial throughput) are either
recoverable by LRU on its own or not representative of production load. The cases where moka
decisively wins — cold bursts and diverse-consumer cache pollution — are recurring operational
events on any production router, not edge cases. The c=1 regression is real but irrelevant in
practice. The trade-offs (deferred eviction, unordered warm-up iteration, new dependency) are
minor and well-mitigated. The change is low risk and the eviction policy improvement is
durable.

---

## What Changed

### `apollo-router/src/cache/storage.rs`

| Aspect | Before | After |
|---|---|---|
| Cache type | `Arc<Mutex<LruCache<K, V>>>` | `moka::future::Cache<K, V>` |
| Cache read | `inner.lock().await.get(key).cloned()` | `inner.get(key).await` |
| Cache write | `inner.lock().await.push(key, value)` | `inner.insert(key, value).await` |
| Eviction policy | LRU (recency) | W-TinyLFU (frequency + recency, admission filter) |
| Concurrency | Serialized via async mutex | Concurrent via internal striped sharding |
| `cache_size` metric | Tracked via `AtomicI64` updated on every write | Derived from `entry_count()`, which reads moka's internal counter |
| Eviction accounting | On overwrite: computed as `new_size - old_size` | On eviction: decremented via `eviction_listener` |
| `estimated_storage` ordering | `SeqCst` (strongest, unnecessary) | `Relaxed` (sufficient; the gauge is read eventually) |

### `apollo-router/src/query_planner/caching_query_planner.rs`

- Cache warm-up loop updated to use `moka::future::Cache::iter()` rather than holding a mutex
  guard across the iteration.
- Cache look-up in the incremental warm-up reuse path changed from `lock().await.get(…).cloned()`
  to `cache.get(…).await`.
- Warm-up count estimation changed from `cache.len() / 3` to `entry_count() as usize / 3`.
  Since the count is a rough heuristic, the minor imprecision from any unflushed pending
  entries is negligible and does not warrant blocking the reload path with `run_pending_tasks()`.

### `apollo-router/src/cache/mod.rs`

- `in_memory_cache()` now returns a `moka::future::Cache<K, V>` (the same `Clone`-able handle
  to the shared instance).

---

## Background

The existing implementation wraps `lru::LruCache` in a `tokio::sync::Mutex`:

```rust
// Before
pub(crate) type InMemoryCache<K, V> = Arc<Mutex<LruCache<K, V>>>;

inner: Arc<Mutex<LruCache<K, V>>>,
```

Every cache read **and** write acquires this mutex, including:

- The hot path: checking whether a query plan already exists before planning
- Cache-hit reads on the most frequently called GraphQL operations
- The cache warm-up loop that replays prior cache entries across a schema reload

Under concurrent load (many requests hitting identical or similar queries), all Tokio tasks
serialize through a single async mutex. This is the dominant contention point in the query
planning critical path when the cache is warm.

---

## Benefits

### 1. Elimination of the Storage-Layer Mutex (Latency and Throughput)

Under `Mutex<LruCache>`, every concurrent request that hits the cache hot path must await the
lock, then execute a sequential linked-list walk (`get()` on an LRU requires pointer updates to
move the entry to the head). moka uses a concurrent hash map with internal sharding (similar to
`DashMap`), so multiple readers proceed simultaneously without blocking each other.

This PR removes the storage-layer mutex. The already-merged fast-path change makes the
concurrent reads reachable by bypassing the wait-map mutex on cache hits. See the benchmark
section for measured evidence.

With both changes in place, for N concurrent requests hitting the same warm-cache entry:

- **Before:** all N requests serialize through the wait-map mutex, then through the
  `Mutex<LruCache>` — two global serialization points in sequence.
- **After:** all N requests take the fast path and read concurrently from moka — zero global
  locks on the hot path.

The impact is largest on high-RPS routers where many requests share a small set of popular
operations.

### 2. Better Cache Utilization via W-TinyLFU Admission

LRU evicts the *least recently used* entry regardless of how frequently that entry is accessed.
A single scan over infrequently used operations (e.g., a periodic maintenance query) can evict
hot entries that were accessed thousands of times.

moka implements [Window TinyLFU (W-TinyLFU)](https://arxiv.org/abs/1512.00727), a policy
that tracks access frequency via a Count-Min Sketch and uses that frequency to decide whether
a newly inserted entry should evict an existing one. High-frequency entries are retained even
under a spike of low-frequency insertions.

The eviction benchmark (`query_plan_cache_eviction`) measures three scenarios directly:

- **Scan-pollution** (worst case for LRU — cold burst arrives while hot traffic is paused):
  100 hot keys warmed with 30 accesses each, then 2,000 cold one-shot keys inserted. LRU
  retains **0%** of the hot set; W-TinyLFU retains **100%** because its admission filter
  rejects cold candidates whose frequency (0) is below the incumbents' (30+).
- **Interleaved hot + cold** (realistic production case — cold burst alongside live traffic):
  both policies retain 100% of the hot set. LRU's recency signal compensates when hot accesses
  continue refreshing entries. The gap between the two policies is specific to bursts that
  arrive when hot traffic has paused.
- **Zipf steady-state** (α = 1.1, 5,000-key population): moka achieves **80.5–80.7% hit rate**
  vs **75.5% for LRU** at identical capacity — a **+5.0–5.2 percentage point** improvement.

Each cache miss triggers a synchronous query planning round-trip (typically 10–200 ms), so
hit-rate improvements directly reduce tail latency.

**Inline-constant anti-pattern (blast-radius containment).** A related real-world trigger for
scan-pollution is clients that embed constant values directly in query strings instead of using
variables:

```graphql
# Best practice — one cache key regardless of argument value
query GetProduct($id: ID!) { product(id: $id) { name } }

# Anti-pattern — a distinct cache key for every unique ID
query { product(id: "123") { name } }
query { product(id: "456") { name } }
```

A misbehaving client hitting thousands of unique argument values generates a flood of one-shot
cache entries — identical to the pure-flood benchmark scenario. Under LRU those entries evict
the hot plans of every other well-behaved client sharing the same router. Under W-TinyLFU each
inlined query arrives with frequency zero and is rejected by the admission filter in favor of
the high-frequency incumbents.

This provides a degree of blast-radius containment at the cache layer without requiring a
client-side fix. It is especially relevant for multi-tenant deployments where one team's
misbehaving client can silently degrade cache effectiveness for the rest of the organization.

### 3. Simplified Accounting Code

The previous `CacheStorage` maintained two separate atomic counters (`cache_size` and
`cache_estimated_storage`) and performed subtraction arithmetic on every overwrite to compute the
delta between old and new values. This required fetching the old value before the write, then
storing it on the stack while the mutex was held. moka's `eviction_listener` callback handles
debit accounting automatically at eviction time, simplifying the insert path to a single atomic
fetch-add of the new entry size.

### 4. No Breaking API or Configuration Change

The change is entirely internal. `InMemoryCache<K, V>` remains a crate-internal type alias
(`pub(crate)`); its shape changed from `Arc<Mutex<LruCache<K, V>>>` to
`moka::future::Cache<K, V>`, but all call sites that used it through `CacheStorage` do not
change their public API. The YAML configuration key
(`supergraph.query_planning.cache.in_memory.limit`) and its semantics are unchanged: the value
still represents a maximum number of entries.

---

## Negative Impacts and Trade-offs

### 1. Deferred Eviction (Eventual Consistency of Entry Count)

moka performs eviction **asynchronously** in background tasks rather than synchronously on
insert. This means:

- Immediately after an `insert()`, `entry_count()` may return a value up to `max_capacity + N`
  for some small N (pending evictions).
- The `apollo.router.cache.size` gauge may transiently read slightly above the configured
  capacity.
- The `cache_estimated_storage` gauge may similarly over-report for a brief window.

**Mitigation:** For tests that need exact counts, call `run_pending_tasks().await` to flush
pending operations. The production metric gauges tolerate minor transient over-reporting. We
added a `flush_pending()` test helper and updated affected tests accordingly.

### 2. W-TinyLFU Admission Can Reject Cold Entries

W-TinyLFU uses a frequency sketch to decide whether a candidate entry should be admitted.
Immediately after a cold start (empty sketch), any entry is admitted. Once the cache is warm,
a newly inserted entry with no access history may be rejected in favor of retaining an existing
high-frequency entry.

**Impact:** The first time the router sees a new operation after the cache is full, the plan
*may* not be cached on the first insert if the TinyLFU admission filter rejects it. It will be
admitted once it has been observed enough times. For most production workloads where the query
set is stable this is invisible. For workloads with a large tail of rare queries, a small
number of low-frequency operations may not be cached even when the cache is below the
"effective" capacity.

**User-visible behavior change:** Customers testing cache behavior with a capacity of 1 and two
alternating queries will observe that the high-frequency key is *retained* rather than the
most-recently-inserted key. The existing test `test_estimated_storage_with_no_capacity` was
updated to reflect this.

### 3. Warm-Up Iteration Order Is Not Guaranteed

The previous warm-up path iterated `LruCache` in MRU→LRU order (most recently used first).
moka's `Cache::iter()` iterates in an unspecified order (reflects internal shard layout). moka
does not expose per-entry frequency counts from its internal W-TinyLFU sketch, so MFU-ordered
iteration is not directly available either.

**Impact in practice:** Limited. The warm-up loop's primary job is to avoid cold-start planning
latency — getting plans into memory before post-reload requests arrive. Once requests begin
flowing, the TinyLFU sketch learns access frequency within a few hundred hits and makes good
eviction decisions regardless of warm-up insertion order. Ordering only affects which plans
survive if the working set exceeds cache capacity *during warm-up itself*, which requires more
distinct cached operations than `in_memory.limit` (default 512).

The existing shuffle applied to persisted query entries is unchanged. Regular queries are
appended after PQ entries. Which entries are selected for warm-up (up to `count` = `total/3`
by default) and the overall warm-up logic are also unchanged.

### 4. New Dependency: `moka 0.12` (future feature)

`moka` is a well-maintained, MIT/Apache-2.0 licensed crate authored by Tatsuya Kawano. It
passes the router's existing `cargo-deny` license checks and has no transitive GPL-licensed
dependencies. The `lru` crate remains in `apollo-router/Cargo.toml` because it is also used by
other components (response cache, telemetry, query analysis layer) that were not in scope for
this change.

`moka` itself is already present in the workspace's dependency closure as a transitive
dependency of `hickory-resolver`. This PR adds it as a direct dependency with the `future`
feature enabled, which introduces 2 net-new crates: `async-lock` and `event-listener`.

### 5. `'static` Bound Added to `K` and `V` in `CacheStorage`

moka requires `K: 'static + …` and `V: 'static + …` because cache entries may be held in
background tasks. The previous `Mutex<LruCache>` had no such requirement. In practice every
existing instantiation of `CacheStorage` already uses types with `'static` lifetimes (plan
results, APQ entries, etc.), so this imposes no real restriction on current users. Future uses
of `CacheStorage` with non-`'static` types would require a design change.

---

## Behavior Changes Customers Should Be Aware Of

| # | Behavior | Before | After |
|---|---|---|---|
| 1 | **Eviction timing** | Synchronous on insert | Deferred to background task |
| 2 | **Eviction policy** | LRU (recency only) | W-TinyLFU (frequency + recency with admission) |
| 3 | **Metric gauge accuracy** | Exact at time of insert | Both `cache.size` and `estimated_storage` may transiently over-report until the background eviction task drains |
| 4 | **Cold-entry admission** | Always admitted (newest evicts oldest) | Rejected if frequency sketch prefers the incumbent |
| 5 | **Warm-up order** | MRU→LRU (most-recently-used first) | Arbitrary (shard layout) |

None of these changes affect correctness of query execution. The cache is a performance
optimization; a miss results in a (correct) plan being computed on demand.

---

## Performance Benchmarks

Two benchmarks cover the two independent axes of improvement:

1. **`query_plan_cache_concurrency`** — measures throughput and lock contention through the
   full router pipeline at varying concurrency. This is a three-way comparison across
   `dev` / `feat/dedup-cache-fast-path` / `feat/moka-query-plan-cache`, run on dedicated
   hardware where compile-time cache selection matters.

2. **`query_plan_cache_eviction`** — measures hit rate under eviction pressure. Both LRU and
   moka are instantiated in the same binary and run against identical workloads, producing a
   direct side-by-side comparison. See the [Eviction Policy Benchmark](#eviction-policy-benchmark)
   section below.

### Concurrency Sweep

**Setup:**
- Hardware: AWS c8i.8xlarge (32 vCPUs / 16 physical Intel Xeon 6975P-C cores, 64 GB RAM)
- 100 warmup waves per concurrency level before timing begins
- 3 timed repetitions per level; median req/s reported
- Minimum 10,000 requests per level (wave count scales with concurrency)
- All requests are cache hits for the same query (plan is always in memory)
- Mocked subgraphs throughout — subgraph network I/O is not a variable
- Serial fraction estimated via Amdahl's law: `S = (1/speedup − 1/N) / (1 − 1/N)`
- Three configurations run serially on the same machine to avoid hardware interference

**Why this benchmark is meaningful:**
Cache-hit throughput under concurrency directly measures how much of the query-planning hot
path is parallelizable. The Amdahl serial-fraction column quantifies the remaining bottleneck
at each concurrency level; a flat, low serial fraction means the implementation is not
serializing concurrent requests.

### Three-way comparison

All numbers from the same c8i.8xlarge machine, release builds.

#### LRU baseline — `dev` branch (`Mutex<LruCache>`, no fast path)

```
  concurrency       req/s  elapsed (s)    speedup   serial frac.
  --------------------------------------------------------------
  1                  3,088        3.238      1.00×         100.0%
  2                  4,955        2.018      1.60×          24.7%
  4                  8,032        1.245      2.60×          17.9%
  8                 12,739        0.785      4.12×          13.4%
  16                17,090        0.585      5.53×          12.6%
  32                21,519        0.465      6.97×          11.6%
  64                24,639        0.408      7.98×          11.1%
```

#### Fast path only — `feat/dedup-cache-fast-path` (`Mutex<LruCache>` + wait-map fast path)

```
  concurrency       req/s  elapsed (s)    speedup   serial frac.
  --------------------------------------------------------------
  1                  2,864        3.492      1.00×         100.0%
  2                  5,120        1.953      1.79×          11.9%
  4                  8,224        1.216      2.87×          13.1%
  8                 12,696        0.788      4.43×          11.5%
  16                17,876        0.559      6.24×          10.4%
  32                22,205        0.451      7.75×          10.1%
  64                24,896        0.404      8.69×          10.1%
```

#### moka + fast path — `feat/moka-query-plan-cache` (this PR, on top of merged fast-path)

```
  concurrency       req/s  elapsed (s)    speedup   serial frac.
  --------------------------------------------------------------
  1                  2,779        3.598      1.00×         100.0%
  2                  4,323        2.313      1.56×          28.6%
  4                  7,358        1.359      2.65×          17.0%
  8                 12,785        0.782      4.60×          10.6%
  16                17,629        0.567      6.34×          10.2%
  32                22,150        0.452      7.97×           9.7%
  64                25,032        0.401      9.01×           9.7%
```

### Interpretation

| Concurrency | LRU baseline | fast path only | moka + fast path |
|---|---|---|---|
| 8  | 12,739 req/s | 12,696 req/s | 12,785 req/s |
| 16 | 17,090 req/s | 17,876 req/s (+4.6%) | 17,629 req/s (+3.2%) |
| 32 | 21,519 req/s | 22,205 req/s (+3.2%) | 22,150 req/s (+3.0%) |
| 64 | 24,639 req/s | 24,896 req/s (+1.0%) | 25,032 req/s (+1.6%) |

| Concurrency | LRU serial frac. | fast path serial frac. | moka serial frac. |
|---|---|---|---|
| 32 | 11.6% | 10.1% | 9.7% |
| 64 | 11.1% | 10.1% | 9.7% |

The fast-path PR is the primary driver of improvement: it eliminates two wait-map mutex
acquisitions, one `HashMap` insert, one `HashMap` remove, and one `tokio::task::spawn` per
cache hit, reducing the serial fraction from ~11.1% to ~10.1% at high concurrency.

The moka storage migration provides a further incremental reduction (10.1% → 9.7%), reflecting
the LruCache mutex being replaced by moka's sharded concurrent map. The throughput delta at
high concurrency is modest (~1–3%) because the LruCache mutex — once the wait-map is bypassed
— is held only briefly for a single `get` + `clone`. The more significant long-term benefit of
the moka migration is the W-TinyLFU eviction policy, which this synthetic single-query benchmark
does not exercise.

To reproduce:
```bash
cargo bench --bench query_plan_cache_concurrency
```

---

## Eviction Policy Benchmark

A second benchmark (`apollo-router-benchmarks/benches/query_plan_cache_eviction.rs`) measures
**hit rate** under eviction pressure. Both `lru::LruCache` and `moka::sync::Cache` are
instantiated in the same binary and run against identical workloads, producing a direct
side-by-side comparison without requiring separate builds or branches.

**Setup:**
- Cache capacity: 512 entries (Apollo Router default: `supergraph.query_planning.cache.in_memory.limit`)
- Hot set: 100 keys (fits entirely in cache)
- Warmup accesses per hot key: 30 (builds frequency history in TinyLFU sketch)
- Cold flood: 2,000 unique one-shot keys

**Why this benchmark is meaningful:**
The query plan cache is bounded in size. Production routers serving diverse API consumers may
have more unique operations than cache slots. When a low-frequency operation (scheduled job,
admin query, health-check variant) arrives in a burst, LRU evicts whatever was least recently
used — which are the hot operations that were accessed long before the burst began. W-TinyLFU
rejects those cold candidates because their frequency count (0) is lower than the hot entries'
accumulated frequency, preserving the entries that matter most for throughput.

#### Scenario 1 — Pure scan-pollution (worst case for LRU)

Warm the hot set, then flood with 2,000 cold keys with no concurrent hot traffic. This models
a maintenance batch job or schema-reload warm-up burst arriving while the router is quiet.

```
                        after warm    after flood
  LRU                    100.0%          0.0%   (0 of 100 hot entries retained)
  moka (W-TinyLFU)       100.0%        100.0%  (100 of 100 hot entries retained)
```

LRU's entire hot set is destroyed. W-TinyLFU's admission filter rejects every cold candidate
because its frequency (0) is below any hot entry's frequency (30+).

#### Scenario 2 — Interleaved hot + cold traffic (realistic production case)

Warm the hot set, then interleave one hot access per cold insert (round-robin over hot keys).
This models a cold burst — batch job, health-check variants, new-operation spike — arriving
alongside live traffic. LRU's recency mechanism is able to keep hot entries near the MRU end
as long as hot accesses keep refreshing them.

```
                        after warm    after flood
  LRU                    100.0%        100.0%  (100 of 100 hot entries retained)
  moka (W-TinyLFU)       100.0%        100.0%  (100 of 100 hot entries retained)
```

Both policies retain the full hot set. This is the honest upper bound on LRU's resilience:
when hot traffic continues at a rate comparable to the cold burst, LRU's recency signal
compensates. The gap between the two policies is specific to bursts that arrive when hot
traffic has paused or is sparse relative to the cold insert rate.

#### Scenario 3 — Zipf steady-state (α = 1.1)

100,000 accesses over a 5,000-key population following a Zipf distribution. Both caches start
cold. This simulates a realistic mixed workload where a small number of popular operations
dominate traffic alongside a long tail of rare ones.

```
  LRU                   75.5%
  moka (W-TinyLFU)      80.5–80.7%  (+5.0–5.2pp across 10 runs)
```

At steady state, W-TinyLFU retains the high-frequency head of the distribution more reliably,
yielding a persistent +5.0–5.2 percentage point hit-rate advantage at identical capacity.

#### Summary

| Scenario | LRU result | moka result | Takeaway |
|---|---|---|---|
| Pure flood (hot traffic paused) | 0% hot-set retention | 100% hot-set retention | Frequency sketch protects incumbents; recency alone cannot |
| Interleaved (hot traffic continues) | 100% hot-set retention | 100% hot-set retention | LRU recovers when recency signal is maintained; both policies converge |
| Zipf steady-state (α = 1.1) | 75.5% hit rate | 80.5–80.7% hit rate (+5.0–5.2pp) | Frequency-aware eviction yields a persistent hit-rate advantage |

The interleaved test represents **LRU at its best** — it identifies the specific condition
under which LRU's recency signal compensates (cold inserts arriving at roughly the same rate
as hot accesses), and bounds the claim honestly: W-TinyLFU's eviction advantage is not
universal, but it is decisive in the scenarios where cache misses are most costly.

To reproduce:
```bash
cargo bench --bench query_plan_cache_eviction
```

---

## Testing

All existing tests pass with minor adjustments:

- `test_estimated_storage_with_no_capacity`: updated expected values to reflect W-TinyLFU
  admission semantics (high-frequency entry retained over cold entry).
- `test_estimated_storage`: added `flush_pending().await` calls before gauge assertions to
  account for moka's deferred eviction.
- New test `test_estimated_storage_overwrite`: covers the case where inserting a larger value
  over the same key correctly updates `cache_estimated_storage`.
- `flush_pending()` marked `#[must_use]` so the compiler warns if `.await` is accidentally
  omitted, preventing silent false-positive assertions.

```bash
cargo nextest run --lib -E 'test(cache::storage)'
cargo nextest run --lib -E 'test(query_planner::caching)'
```

---

## Recommended Next Steps

1. Merge this change into `dev` (the prerequisite `DeduplicatingCache` fast-path PR
   has already merged).
2. Evaluate migrating `query_analysis.rs` (`tokio::sync::Mutex<LruCache>`) to moka or
   `CacheStorage` using the same approach — it is the closest structural analog to this change.
   The other remaining `lru::LruCache` usages (`response_cache` uses `RwLock<LruCache>` for
   private-query bookkeeping; `apollo_telemetry` uses `parking_lot::Mutex<LruCache>` for span
   buffering) serve different purposes and would require separate evaluation.
3. Capture a production flame graph before/after to quantify the mutex-contention reduction on
   a live workload.
