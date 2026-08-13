//! Threaded stress over the `Send + Sync` surface: many threads concurrently
//! clone, share, read, mutate (copy-on-write), serialize, detach, and drop
//! documents and compositions whose arenas reference each other.
//!
//! The crate has no hand-rolled synchronization — every cross-thread
//! lifetime is a `std::sync::Arc<Arena>` — so the property under test is
//! that arbitrary interleavings of clone/drop/read across arenas linked by
//! foreign references never produce a torn read, a use-after-free (Miri and
//! ASan runs of this suite would flag one), or a wrong serialization.

use apollo_json::{Value, ValueBuilder};

fn entity_doc(seed: usize) -> Value {
    let mut json = format!(r#"{{"meta":{{"seed":{seed}}},"items":["#);
    for i in 0..64 {
        if i > 0 {
            json.push(',');
        }
        json.push_str(&format!(
            r#"{{"id":{i},"name":"entity-{seed}-{i}","tags":["a","b"]}}"#
        ));
    }
    json.push_str("]}");
    Value::parse(json.into_bytes()).expect("fixture parses")
}

/// Composes fragments of every source into one document, exercising adopt,
/// merge, in-place mutation, and seal.
fn compose(sources: &[Value], round: usize) -> Value {
    let mut builder = ValueBuilder::new();
    for (i, source) in sources.iter().enumerate() {
        let item = source
            .get("items")
            .and_then(|items| items.index((round + i) % 64))
            .expect("item exists");
        builder
            .set(format!("frag{i}").as_str(), item)
            .expect("set resolves");
    }
    builder.merge(&sources[round % sources.len()]);
    builder.set("round", round as i64).expect("root is object");
    builder.seal()
}

/// Every thread hammers the same shared sources: reads and serializations
/// must keep returning the pre-captured bytes while other threads clone,
/// compose, copy-on-write mutate, detach, and drop references to the same
/// arenas in arbitrary interleavings.
#[test]
fn shared_documents_survive_concurrent_use() {
    const THREADS: usize = 8;
    const ROUNDS: usize = if cfg!(miri) { 2 } else { 48 };

    let sources: Vec<Value> = (0..3).map(entity_doc).collect();
    let expected: Vec<Vec<u8>> = sources.iter().map(Value::to_vec).collect();

    std::thread::scope(|scope| {
        for thread in 0..THREADS {
            let sources = sources.clone();
            let expected = &expected;
            scope.spawn(move || {
                for round in 0..ROUNDS {
                    // Untouched shared documents serialize byte-identically
                    // regardless of what other threads are doing.
                    let probe = (thread + round) % sources.len();
                    assert_eq!(sources[probe].to_vec(), expected[probe]);

                    let composed = compose(&sources, round);
                    let composed_bytes = composed.to_vec();

                    // Copy-on-write mutation of a shared source is isolated.
                    let mut editor = sources[probe].clone().edit();
                    editor
                        .set_path(&["meta".into(), "touched_by".into()], thread as i64)
                        .expect("path resolves");
                    let edited = editor.seal();
                    assert_ne!(edited.to_vec(), expected[probe]);
                    assert_eq!(sources[probe].to_vec(), expected[probe]);

                    // A detached fragment and a compacted composition stay
                    // byte-identical and outlive every source reference.
                    let fragment = composed.get("frag0").expect("fragment exists");
                    let fragment_bytes = fragment.to_vec();
                    let detached = fragment.compact();
                    let retained = composed.clone().into_self_contained();
                    drop((fragment, composed, edited));
                    assert_eq!(detached.to_vec(), fragment_bytes);
                    assert_eq!(retained.to_vec(), composed_bytes);

                    // Streaming serialization shares the arenas once more.
                    let streamed: Vec<u8> = retained
                        .into_chunks(97)
                        .flat_map(|chunk| chunk.to_vec())
                        .collect();
                    assert_eq!(streamed, composed_bytes);
                }
            });
        }
    });
}

/// The last reference to an arena chain drops on a different thread than the
/// one that built it: compositions referencing all sources migrate to worker
/// threads, the spawning thread drops its own references first, and each
/// worker frees the whole chain.
#[test]
fn last_reference_can_drop_on_any_thread() {
    const THREADS: usize = 8;

    let sources: Vec<Value> = (0..3).map(entity_doc).collect();
    let compositions: Vec<(Value, Vec<u8>)> = (0..THREADS)
        .map(|round| {
            let composed = compose(&sources, round);
            let bytes = composed.to_vec();
            (composed, bytes)
        })
        .collect();
    drop(sources); // Workers now hold the only pins on the source arenas.

    let workers: Vec<std::thread::JoinHandle<()>> = compositions
        .into_iter()
        .map(|(composed, bytes)| {
            std::thread::spawn(move || {
                assert_eq!(composed.to_vec(), bytes);
            })
        })
        .collect();
    for worker in workers {
        worker.join().expect("worker completes");
    }
}
