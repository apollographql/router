//! String-scanner microbenchmark: parse and serialize throughput with
//! whichever scanner this process selected.
//!
//! Run once per scanner and compare (each run is its own process, because
//! the choice is made once per process):
//!
//! ```sh
//! for s in portable sse2 avx2; do
//!   APOLLO_JSON_SCANNER=$s cargo bench --bench scanner
//! done
//! ```
//!
//! Each line reports the scanner actually in use, so an `avx2` request on a
//! CPU without AVX2 shows up as `sse2`. Timings under emulation (Rosetta,
//! QEMU) say nothing about real hardware.

use std::hint::black_box;
use std::time::{Duration, Instant};

use apollo_json::Value;
use serde::{Deserialize, Serialize};

/// A product with reviews, shaped like a federated entity response: many
/// short keys and ids, plus review bodies with an escape in each.
#[derive(Serialize, Deserialize)]
struct Product {
    __typename: String,
    upc: String,
    name: String,
    price: u64,
    reviews: Vec<Review>,
}

#[derive(Serialize, Deserialize)]
struct Review {
    id: String,
    body: String,
    author: Author,
}

#[derive(Serialize, Deserialize)]
struct Author {
    __typename: String,
    id: String,
    name: String,
}

/// A document whose strings are long and clean, so scans run the bulk loops.
#[derive(Serialize, Deserialize)]
struct Article {
    id: String,
    title: String,
    body: String,
}

fn products(count: usize) -> Vec<Product> {
    (0..count)
        .map(|i| Product {
            __typename: "Product".into(),
            upc: format!("upc-{i}"),
            name: format!("Product {i}"),
            price: 1000 + i as u64,
            reviews: (0..2)
                .map(|n| Review {
                    id: format!("upc-{i}-r{n}"),
                    body: format!("Review {n} of upc-{i}: café \"great\" value"),
                    author: Author {
                        __typename: "User".into(),
                        id: format!("upc-{i}-u{n}"),
                        name: format!("User upc-{i}-u{n}"),
                    },
                })
                .collect(),
        })
        .collect()
}

/// Deterministic prose: words of 2–9 letters, `escape_every` words between
/// quoted phrases (0 for none).
fn prose(len: usize, seed: u64, escape_every: usize) -> String {
    let mut state = seed | 1;
    let mut text = String::with_capacity(len + 16);
    let mut words = 0;
    while text.len() < len {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let word_len = 2 + (state % 8) as usize;
        for k in 0..word_len {
            text.push((b'a' + ((state >> (8 + k * 5)) % 26) as u8) as char);
        }
        words += 1;
        if escape_every > 0 && words % escape_every == 0 {
            text.push_str(" \"quoted\"\n");
        } else {
            text.push(' ');
        }
    }
    text
}

fn articles(count: usize, body_len: usize, escape_every: usize) -> Vec<Article> {
    (0..count)
        .map(|i| Article {
            id: format!("article-{i}"),
            title: prose(60, i as u64 + 1, 0),
            body: prose(body_len, i as u64 + 7, escape_every),
        })
        .collect()
}

/// Median time per call over `samples` batches, each run for at least
/// `batch`.
fn time(mut f: impl FnMut(), samples: usize, batch: Duration) -> Duration {
    for _ in 0..3 {
        f();
    }
    let mut per_call = Vec::with_capacity(samples);
    for _ in 0..samples {
        let mut calls = 0u32;
        let started = Instant::now();
        while started.elapsed() < batch {
            f();
            calls += 1;
        }
        per_call.push(started.elapsed() / calls);
    }
    per_call.sort();
    per_call[samples / 2]
}

fn report(scanner: &str, payload: &str, operation: &str, bytes: usize, elapsed: Duration) {
    let mb_per_s = bytes as f64 / elapsed.as_secs_f64() / 1e6;
    println!(
        "{scanner:<8} {payload:<20} {operation:<16} {bytes:>9} B {:>10.1} µs {mb_per_s:>9.1} MB/s",
        elapsed.as_secs_f64() * 1e6
    );
}

fn bench<T: Serialize + for<'de> Deserialize<'de>>(scanner: &str, payload: &str, data: &T) {
    const SAMPLES: usize = 15;
    const BATCH: Duration = Duration::from_millis(100);

    // Parsing: the input is the serialized document. Cloning `Bytes` only
    // bumps a refcount, so the timed parse copies nothing first.
    let input = bytes::Bytes::from(apollo_json::to_value(data).unwrap().to_vec());
    let elapsed = time(
        || drop(black_box(Value::parse(black_box(input.clone())).unwrap())),
        SAMPLES,
        BATCH,
    );
    report(scanner, payload, "parse", input.len(), elapsed);
    let elapsed = time(
        || {
            drop(black_box(
                apollo_json::from_slice::<T>(black_box(&input)).unwrap(),
            ))
        },
        SAMPLES,
        BATCH,
    );
    report(scanner, payload, "from_slice", input.len(), elapsed);

    // Serializing: a value built from Rust data holds owned strings, which
    // the writer scans for bytes to escape. (Parsed strings are copied out
    // verbatim and never scanned.)
    let owned = apollo_json::to_value(data).unwrap();
    let mut out = Vec::with_capacity(input.len() * 2);
    let elapsed = time(
        || {
            out.clear();
            black_box(&owned).serialize_into(&mut out);
            black_box(&out);
        },
        SAMPLES,
        BATCH,
    );
    report(scanner, payload, "serialize owned", out.len(), elapsed);
}

fn main() {
    let scanner = apollo_json::string_scanner();
    println!(
        "{:<8} {:<20} {:<16} {:>11} {:>13} {:>14}",
        "scanner", "payload", "operation", "size", "median", "throughput"
    );
    bench(scanner, "entities_1000", &products(1000));
    bench(scanner, "long_clean_strings", &articles(200, 4096, 0));
    bench(scanner, "long_escaped_strings", &articles(200, 4096, 8));
}
