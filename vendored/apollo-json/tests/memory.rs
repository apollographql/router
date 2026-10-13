//! Arena-liveness behavior, observed through a counting global allocator:
//! sharing pins whole arenas, dropping the last handle frees them, and
//! `detach()` severs the pin.

// The assertions here are deltas of global live-heap counts. Miri's
// allocation behavior (its own base allocations, no real `realloc` growth
// pattern) makes those deltas meaningless, so this suite is native-only;
// Miri covers the same code paths through the other suites.
#![cfg(not(miri))]

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicIsize, Ordering};

use std::borrow::Cow;

use apollo_json::{NewValue, PathSegment, Value, ValueBuilder};

mod common;
use common::parse;

struct CountingAllocator;

static LIVE: AtomicIsize = AtomicIsize::new(0);
static ALLOCS: AtomicIsize = AtomicIsize::new(0);

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
            LIVE.fetch_add(layout.size() as isize, Ordering::Relaxed);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        LIVE.fetch_sub(layout.size() as isize, Ordering::Relaxed);
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let new_ptr = unsafe { System.realloc(ptr, layout, new_size) };
        if !new_ptr.is_null() {
            LIVE.fetch_add(
                new_size as isize - layout.size() as isize,
                Ordering::Relaxed,
            );
        }
        new_ptr
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

fn live() -> isize {
    LIVE.load(Ordering::Relaxed)
}

/// Serializes the tests that assert on global live-heap counts; parallel
/// allocations from other tests would make the deltas meaningless.
static HEAP_MEASUREMENT: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// A document with one large entity list so arena retention is visible over
/// allocator noise.
fn large_doc_json() -> String {
    let mut json = String::from(r#"{"meta":{"kind":"fixture"},"items":["#);
    for i in 0..5000 {
        if i > 0 {
            json.push(',');
        }
        json.push_str(&format!(
            r#"{{"id":{i},"name":"entity-{i}","tags":["alpha","beta"]}}"#
        ));
    }
    json.push_str("]}");
    json
}

/// Dropping the source document does not free the arena while a subtree
/// handle exists; dropping the handle does.
#[test]
fn arena_stays_alive_until_last_subtree_handle_drops() {
    let _guard = HEAP_MEASUREMENT.lock().unwrap();
    let json = large_doc_json();
    let baseline = live();

    let doc = parse(&json);
    let arena_size = live() - baseline;
    assert!(
        arena_size > 200_000,
        "expected a large arena, got {arena_size} bytes"
    );

    // Hold one ~60-byte entity from the middle, then drop the document.
    let one_item = doc
        .get("items")
        .and_then(|items| items.index(2500))
        .expect("entity exists");
    drop(doc);

    let while_pinned = live() - baseline;
    assert!(
        while_pinned > arena_size / 2,
        "arena should stay pinned by the subtree handle: \
         arena_size={arena_size} while_pinned={while_pinned}"
    );
    assert_eq!(
        one_item.value().get("id").and_then(|v| v.as_i64()),
        Some(2500)
    );

    drop(one_item);
    let after_release = live() - baseline;
    assert!(
        after_release < arena_size / 10,
        "arena should be freed once the last handle drops: \
         arena_size={arena_size} after_release={after_release}"
    );
}

/// `detach()` copies the subtree into a minimal arena and releases the
/// source arena, while preserving the serialized bytes.
#[test]
fn detach_severs_the_arena_pin() {
    let _guard = HEAP_MEASUREMENT.lock().unwrap();
    let json = large_doc_json();
    let baseline = live();

    let doc = parse(&json);
    let arena_size = live() - baseline;
    let handle = doc
        .get("items")
        .and_then(|items| items.index(2500))
        .expect("entity exists");
    let expected = handle.to_vec();

    let detached = handle.compact();
    drop((handle, doc));

    let after_detach = live() - baseline;
    assert!(
        after_detach < arena_size / 10,
        "detach must release the source arena: \
         arena_size={arena_size} after_detach={after_detach}"
    );
    assert_eq!(detached.to_vec(), expected, "detach preserves bytes");
}

/// A composition of three source documents keeps all three arenas alive and
/// releases them together when it drops.
#[test]
fn composition_pins_and_releases_all_source_arenas() {
    let _guard = HEAP_MEASUREMENT.lock().unwrap();
    let json = large_doc_json();
    let baseline = live();

    let sources: Vec<Value> = (0..3).map(|_| parse(&json)).collect();
    let per_arena = (live() - baseline) / 3;

    let mut builder = ValueBuilder::new();
    for (i, source) in sources.iter().enumerate() {
        let entity = source
            .get("items")
            .and_then(|items| items.index(i * 100))
            .expect("entity exists");
        builder
            .set_path(
                &[PathSegment::Key(&format!("subgraph{i}"))],
                NewValue::Node(entity),
            )
            .expect("path resolves");
    }
    let composed = builder.seal();
    drop(sources);

    assert!(
        live() - baseline > per_arena * 2,
        "all source arenas must stay alive through the composition"
    );
    let out = String::from_utf8(composed.to_vec()).unwrap();
    for i in 0..3 {
        assert!(
            out.contains(&format!(r#""name":"entity-{}""#, i * 100)),
            "{out}"
        );
    }

    drop(composed);
    assert!(
        live() - baseline < per_arena / 2,
        "dropping the composition must release every source arena"
    );
}

/// Steady-state recycled parses allocate almost nothing: after a warm-up
/// cycle, a parse costs the document's `Arc` plus the parser stack, while
/// a fresh parse allocates dozens of chunks and scratch buffers.
#[test]
fn recycled_parses_reach_allocation_steady_state() {
    use apollo_json::{ParseBuffers, ParseOptions};

    let _guard = HEAP_MEASUREMENT.lock().unwrap();
    let json = large_doc_json();
    let options = ParseOptions::default();
    const ROUNDS: isize = 5;

    // Inputs are pre-cloned so the measurement sees only the parses.
    let mut inputs: Vec<Vec<u8>> = (0..ROUNDS).map(|_| json.clone().into_bytes()).collect();

    let before = ALLOCS.load(Ordering::Relaxed);
    for input in inputs.drain(..) {
        drop(Value::parse(input).expect("fixture parses"));
    }
    let fresh_per_parse = (ALLOCS.load(Ordering::Relaxed) - before) / ROUNDS;

    let mut buffers = ParseBuffers::new();
    let mut inputs: Vec<Vec<u8>> = (0..ROUNDS + 1).map(|_| json.clone().into_bytes()).collect();
    // Warm-up parse fills the buffers.
    Value::parse_with_buffers(inputs.pop().expect("warm-up input"), &options, &mut buffers)
        .expect("fixture parses")
        .recycle(&mut buffers);

    let before = ALLOCS.load(Ordering::Relaxed);
    for input in inputs.drain(..) {
        let doc = Value::parse_with_buffers(input, &options, &mut buffers).expect("parses");
        assert!(doc.recycle(&mut buffers));
    }
    let recycled_per_parse = (ALLOCS.load(Ordering::Relaxed) - before) / ROUNDS;

    assert!(
        fresh_per_parse >= 20,
        "expected a fresh parse to allocate chunks and scratch, got {fresh_per_parse}"
    );
    assert!(
        recycled_per_parse <= 8,
        "steady-state recycled parses should be nearly allocation-free, \
         got {recycled_per_parse} (fresh: {fresh_per_parse})"
    );
}

/// The retention boundary: already-self-contained documents pass through
/// without a single allocation; compositions compact into an arena that
/// pins nothing, releasing every source arena.
#[test]
fn into_self_contained_is_the_retention_boundary() {
    let _guard = HEAP_MEASUREMENT.lock().unwrap();
    let json = large_doc_json();
    let baseline = live();

    // A parsed document owns everything it references: identity, for free.
    let doc = parse(&json);
    assert!(doc.is_self_contained());
    let allocs_before = ALLOCS.load(Ordering::Relaxed);
    let doc = doc.into_self_contained();
    assert_eq!(
        ALLOCS.load(Ordering::Relaxed) - allocs_before,
        0,
        "the no-op path must not allocate"
    );

    // A composition pins its sources until it crosses the boundary.
    let mut builder = ValueBuilder::new();
    builder
        .set_path(
            &[PathSegment::Key("kept")],
            NewValue::Node(
                doc.get("items")
                    .and_then(|items| items.index(2500))
                    .expect("entity exists"),
            ),
        )
        .expect("path resolves");
    let composed = builder.seal();
    assert!(!composed.is_self_contained());
    let expected = composed.to_vec();

    let retained = composed.into_self_contained();
    drop(doc);

    assert!(retained.is_self_contained());
    assert_eq!(retained.to_vec(), expected, "compaction preserves bytes");
    let after = live() - baseline;
    assert!(
        after < 50_000,
        "the source arena must be released once the boundary is crossed: {after} bytes live"
    );
}

/// The reason pending trees exist: assembling nested structure through one
/// builder allocates a bounded amount regardless of how many containers the
/// structure has, where sealing a document per container scales with them.
///
/// A response formatter is the caller that cares. Left to seal per level, it
/// paid an arena, an `Arc`, a slab and a separate drop for every object in the
/// response; the profile put roughly half its CPU there.
#[test]
fn pending_trees_do_not_allocate_per_container() {
    const CONTAINERS: usize = 200;

    /// One pending tree of `CONTAINERS` objects, written through one builder.
    fn pending(keys: &[String]) -> Value {
        let members = keys
            .iter()
            .map(|key| {
                (
                    std::borrow::Cow::Borrowed(key.as_str()),
                    NewValue::Object(vec![("n".into(), NewValue::String(Cow::Borrowed(key)))]),
                )
            })
            .collect();
        let mut builder = ValueBuilder::new();
        builder.set("items", NewValue::Object(members)).unwrap();
        builder.seal()
    }

    /// The same shape, sealing a document per object and adopting each into its
    /// parent — what the API forced before pending trees existed.
    fn per_container(keys: &[String]) -> Value {
        let mut outer = ValueBuilder::new();
        let mut items = ValueBuilder::new();
        for key in keys {
            let mut inner = ValueBuilder::new();
            inner.set("n", key.as_str()).unwrap();
            items.set(key.as_str(), inner.seal()).unwrap();
        }
        outer.set("items", items.seal()).unwrap();
        outer.seal()
    }

    // Distinct keys, allocated before measuring so the counts cover only the
    // document build.
    let keys: Vec<String> = (0..CONTAINERS).map(|i| format!("k{i}")).collect();

    let _guard = HEAP_MEASUREMENT.lock().unwrap();

    // Warm up both paths so first-call lazies are not counted.
    drop(pending(&keys));
    drop(per_container(&keys));

    let before = ALLOCS.load(Ordering::Relaxed);
    let pending_doc = pending(&keys);
    let pending_allocs = ALLOCS.load(Ordering::Relaxed) - before;

    let before = ALLOCS.load(Ordering::Relaxed);
    let per_container_doc = per_container(&keys);
    let per_container_allocs = ALLOCS.load(Ordering::Relaxed) - before;

    // Both produce the same members, so this is a like-for-like comparison.
    let members = |doc: &Value| doc.get("items").unwrap().len();
    assert_eq!(members(&pending_doc), Some(CONTAINERS));
    assert_eq!(members(&per_container_doc), Some(CONTAINERS));

    // Per-container sealing allocates several times per container: an arena,
    // its slabs, the Arc, the foreign-table entry.
    assert!(
        per_container_allocs >= CONTAINERS as isize,
        "expected per-container sealing to allocate at least once per container, \
         got {per_container_allocs} for {CONTAINERS}"
    );
    // The pending tree's own allocations are the two scratch buffers and the
    // arena's amortized growth -- everything else is the caller's `Vec` per
    // object, one each, which is the pending tree's own data. So the builder
    // contributes nothing per container, and the total stays far below the
    // per-container path even counting those Vecs.
    assert!(
        pending_allocs < CONTAINERS as isize * 2,
        "a pending tree of {CONTAINERS} objects allocated {pending_allocs} times; \
         beyond one `Vec` per object from the caller, writing it should not \
         allocate per container"
    );
    assert!(
        pending_allocs * 4 < per_container_allocs,
        "a pending tree of {CONTAINERS} objects allocated {pending_allocs} times \
         against {per_container_allocs} for per-container sealing; the pending \
         path is supposed to be substantially cheaper"
    );
}

/// Writing numbers does not allocate per number. The obvious spelling —
/// formatting through `to_string()` — costs one `String` per number written,
/// which for a document that is mostly numbers is its whole per-member cost.
#[test]
fn writing_numbers_does_not_allocate_per_number() {
    const NUMBERS: usize = 200;

    let keys: Vec<String> = (0..NUMBERS).map(|i| format!("k{i}")).collect();
    let build = |keys: &[String]| {
        let members = keys
            .iter()
            .enumerate()
            .map(|(i, key)| {
                let value = if i % 2 == 0 {
                    NewValue::Int(i as i64 * 1_000_003)
                } else {
                    NewValue::Float(i as f64 + 0.5)
                };
                (Cow::Borrowed(key.as_str()), value)
            })
            .collect();
        let mut builder = ValueBuilder::new();
        builder.set("n", NewValue::Object(members)).unwrap();
        builder.seal()
    };

    let _guard = HEAP_MEASUREMENT.lock().unwrap();
    drop(build(&keys));

    let before = ALLOCS.load(Ordering::Relaxed);
    let doc = build(&keys);
    let allocs = ALLOCS.load(Ordering::Relaxed) - before;

    // Sanity-check the numbers actually landed, and that floats and integers
    // are formatted the way serde_json formats them.
    let numbers = doc.get("n").expect("members were written");
    assert_eq!(numbers.get("k0").and_then(|v| v.as_i64()), Some(0));
    assert_eq!(numbers.get("k1").and_then(|v| v.as_f64()), Some(1.5));
    assert!(
        doc.to_string().contains(r#""k3":3.5"#),
        "{}",
        doc.to_string()
    );

    assert!(
        allocs < NUMBERS as isize / 4,
        "writing {NUMBERS} numbers allocated {allocs} times; number text is \
         supposed to be formatted into a stack buffer, not a String per number"
    );
}
