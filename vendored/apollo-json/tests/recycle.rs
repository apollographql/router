//! Parse-buffer recycling: recycled storage must be invisible in behavior —
//! byte-identical output, same cap enforcement, same acceptance — and only
//! reclaimable from the last reference.

use apollo_json::{JsonError, ParseBuffers, ParseOptions, Value};

/// Documents of very different shapes and sizes, cycled twice through one
/// set of buffers; every parse must match a fresh parse byte-for-byte.
#[test]
fn recycled_parses_match_fresh_parses() {
    let big_array = format!("[{}]", vec!["123"; 20_000].join(","));
    let wide: String = format!(
        "{{{}}}",
        (0..200)
            .map(|i| format!(r#""key{i}":"value{i}""#))
            .collect::<Vec<_>>()
            .join(",")
    );
    let escapes = r#"{"esc":"a\nbA😀","num":1.5e-3}"#;
    let cases: Vec<&[u8]> = vec![
        br#"{"a":1}"#,
        big_array.as_bytes(),
        escapes.as_bytes(),
        wide.as_bytes(),
        b"null",
        br#"{"dup":1,"dup":2}"#,
        br#"[[[[[[[[[[1]]]]]]]]]]"#,
    ];

    let options = ParseOptions::default();
    let mut buffers = ParseBuffers::new();
    for round in 0..2 {
        for case in &cases {
            let fresh = Value::parse(case.to_vec()).expect("fresh parse succeeds");
            let recycled = Value::parse_with_buffers(case.to_vec(), &options, &mut buffers)
                .expect("recycled parse succeeds");
            assert_eq!(
                recycled.to_vec(),
                fresh.to_vec(),
                "round {round}: {}",
                String::from_utf8_lossy(&case[..case.len().min(40)])
            );
            assert!(recycled.recycle(&mut buffers), "sole reference recycles");
        }
    }
}

/// Clones and subtree handles both count as references; a shared document
/// is dropped without donating its storage.
#[test]
fn recycle_requires_the_last_reference() {
    let mut buffers = ParseBuffers::new();
    let doc = Value::parse(br#"{"a":1}"#.to_vec()).unwrap();
    let clone = doc.clone();
    assert!(!doc.recycle(&mut buffers), "shared arena must not recycle");
    assert!(clone.recycle(&mut buffers), "last reference recycles");

    let doc = Value::parse(br#"{"a":[1,2]}"#.to_vec()).unwrap();
    let handle = doc.get("a").unwrap();
    assert!(
        !doc.recycle(&mut buffers),
        "a subtree handle keeps the arena un-recyclable"
    );
    assert_eq!(handle.to_vec(), b"[1,2]", "the handle stays valid");
}

/// Caps apply unchanged to recycled parses, and recycled storage larger
/// than the cap is discarded rather than failing a parse a fresh arena
/// would accept.
#[test]
fn caps_apply_to_recycled_parses() {
    let options = ParseOptions::default()
        .with_max_arena_bytes(64 * 1024)
        .with_max_depth(4);
    let mut buffers = ParseBuffers::new();

    let small = br#"{"a":[1,2]}"#.to_vec();
    let doc = Value::parse_with_buffers(small.clone(), &options, &mut buffers).unwrap();
    doc.recycle(&mut buffers);

    let big = format!("[{}]", vec!["1"; 30_000].join(","));
    assert!(matches!(
        Value::parse_with_buffers(big.into_bytes(), &options, &mut buffers),
        Err(JsonError::ArenaLimitExceeded { .. })
    ));
    assert!(matches!(
        Value::parse_with_buffers(b"[[[[[1]]]]]".to_vec(), &options, &mut buffers),
        Err(JsonError::DepthLimitExceeded { .. })
    ));

    // Storage recycled under a generous cap does not poison a tighter one:
    // grow the buffers past 64 KiB, then parse under the tight cap again.
    let generous = ParseOptions::default();
    let filler = format!("[{}]", vec!["12345"; 40_000].join(","));
    let doc = Value::parse_with_buffers(filler.into_bytes(), &generous, &mut buffers).unwrap();
    doc.recycle(&mut buffers);
    let doc = Value::parse_with_buffers(small.clone(), &options, &mut buffers)
        .expect("oversized recycled storage is discarded, not fatal");
    assert_eq!(doc.to_vec(), small);
}

/// A parse failure part-way through a document must not poison the buffers
/// for the next parse.
#[test]
fn failed_parses_leave_buffers_reusable() {
    let options = ParseOptions::default();
    let mut buffers = ParseBuffers::new();
    for _ in 0..2 {
        assert!(
            Value::parse_with_buffers(br#"{"a":[1,2,"#.to_vec(), &options, &mut buffers).is_err()
        );
        let doc = Value::parse_with_buffers(br#"{"a":[1,2]}"#.to_vec(), &options, &mut buffers)
            .expect("buffers stay usable after a failed parse");
        assert_eq!(doc.to_vec(), br#"{"a":[1,2]}"#);
        doc.recycle(&mut buffers);
    }
}

/// Recycling a composition releases the arenas it pinned; the sources'
/// content stays reachable through their own documents.
#[test]
fn recycling_a_composition_releases_its_pins() {
    let source = Value::parse(br#"{"sub":{"k":"v"}}"#.to_vec()).unwrap();
    let mut builder = apollo_json::ValueBuilder::new();
    builder.set("adopted", source.get("sub").unwrap()).unwrap();
    let composed = builder.seal();
    assert!(!composed.is_self_contained());

    let mut buffers = ParseBuffers::new();
    assert!(composed.recycle(&mut buffers));
    assert_eq!(source.to_vec(), br#"{"sub":{"k":"v"}}"#);

    let doc = Value::parse_with_buffers(
        br#"{"fresh":true}"#.to_vec(),
        &ParseOptions::default(),
        &mut buffers,
    )
    .unwrap();
    assert_eq!(doc.to_vec(), br#"{"fresh":true}"#);
}

/// The typed streaming entry point recycles internally; a failure at either
/// stage — syntax during the parse, or a shape mismatch during typed
/// deserialization — must leave the buffers usable for the next call.
#[test]
fn failed_typed_deserializations_leave_buffers_reusable() {
    let options = ParseOptions::default();
    let mut buffers = ParseBuffers::new();
    for _ in 0..2 {
        let error =
            apollo_json::from_slice_with_buffers::<Vec<u32>>(b"[1,2,", &options, &mut buffers)
                .unwrap_err();
        assert!(matches!(error, JsonError::Syntax { .. }), "{error}");

        let error =
            apollo_json::from_slice_with_buffers::<Vec<u32>>(br#"{"a":1}"#, &options, &mut buffers)
                .unwrap_err();
        assert!(
            matches!(error, JsonError::Deserialization { .. }),
            "{error}"
        );

        let ids: Vec<u32> =
            apollo_json::from_slice_with_buffers(b"[1,2,3]", &options, &mut buffers)
                .expect("buffers stay usable after failed calls");
        assert_eq!(ids, [1, 2, 3]);
    }
}
