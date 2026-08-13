//! Serialization fidelity: untouched documents round-trip byte-identically,
//! leaves decode lazily, and detach/convert preserve content.

use apollo_json::Value;

mod common;
use common::parse;

/// The byte-identical bar: raw number formats, escape spellings, and key
/// order all survive an untouched round trip.
#[test]
fn untouched_documents_round_trip_byte_identically() {
    let cases: &[&[u8]] = &[
        br#"{"b":1e2,"a":1.50,"big":9007199254740993,"neg":-0.0e-5}"#,
        r#"{"s":"passé","t":"plain","pair":"😀","mix":"a\/b\n"}"#.as_bytes(),
        br#"{"z":1,"a":2,"m":{"y":[],"x":{}}}"#,
        br#"[3.141592653589793238462643383279,1E+2,18446744073709551615]"#,
        b"null",
    ];
    for case in cases {
        assert_eq!(
            parse(case).to_vec(),
            *case,
            "{}",
            String::from_utf8_lossy(case)
        );
    }
}

/// `Debug` renders the serialized JSON, so documents and handles are
/// inspectable in logs and test failures.
#[test]
fn debug_output_renders_the_json() {
    let doc = parse(br#"{"a":1}"#);
    assert_eq!(format!("{doc:?}"), r#"Value({"a":1})"#);
    assert_eq!(format!("{:?}", doc), r#"Value({"a":1})"#);
    assert_eq!(
        format!("{:?}", doc.value().get("a").unwrap()),
        "ValueRef(1)"
    );
}

/// Lazy leaf decoding: numbers parse and strings unescape on access.
#[test]
fn leaves_decode_lazily_on_access() {
    let doc = parse(r#"{"i":-42,"u":18446744073709551615,"f":1.5e2,"s":"passé"}"#.as_bytes());
    let root = doc.value();
    assert_eq!(root.get("i").and_then(|v| v.as_i64()), Some(-42));
    assert_eq!(root.get("i").and_then(|v| v.as_u64()), None);
    assert_eq!(root.get("u").and_then(|v| v.as_u64()), Some(u64::MAX));
    assert_eq!(root.get("u").and_then(|v| v.as_i64()), None);
    assert_eq!(root.get("f").and_then(|v| v.as_f64()), Some(150.0));
    assert_eq!(root.get("f").and_then(|v| v.as_i64()), None);
    assert_eq!(
        root.get("s").and_then(|v| v.as_str()).as_deref(),
        Some("pass\u{e9}")
    );
    assert_eq!(root.get("f").and_then(|v| v.raw_number()), Some("1.5e2"));
}

/// Paths mix keys and indexes; any segment that does not resolve — missing
/// key, out-of-range index, or a segment applied to the wrong shape — yields
/// `None`.
#[test]
fn get_path_resolves_keys_and_indexes() {
    use apollo_json::PathSegment;

    let doc = parse(r#"{"items":[{"id":7,"tags":["a"]}],"n":1}"#.as_bytes());
    let root = doc.value();
    let path = |segments: &[PathSegment<'_>]| root.get_path(segments);

    assert_eq!(
        path(&["items".into(), 0.into(), "id".into()]).and_then(|v| v.as_i64()),
        Some(7)
    );
    assert_eq!(
        path(&["items".into(), 0.into(), "tags".into(), 0.into()])
            .and_then(|v| v.as_str())
            .as_deref(),
        Some("a")
    );
    // The empty path names the value itself.
    assert!(path(&[]).is_some_and(|v| v.is_object()));
    assert!(path(&["missing".into()]).is_none());
    assert!(path(&["items".into(), 1.into()]).is_none());
    assert!(path(&["n".into(), "x".into()]).is_none(), "key on a scalar");
    assert!(
        path(&["items".into(), "x".into()]).is_none(),
        "key on an array"
    );
    assert!(path(&[0.into()]).is_none(), "index on an object");

    // The owned walk agrees with the borrowed one.
    let handle = doc;
    assert_eq!(
        handle
            .get_path(&["items".into(), 0.into(), "id".into()])
            .and_then(|v| v.as_i64()),
        Some(7)
    );
    assert!(handle.get_path(&["missing".into()]).is_none());
}

/// Scalar comparisons read through the lazy accessors: numbers compare at
/// the width of the scalar (`as_i64`/`as_u64`/`as_f64`), strings by their
/// unescaped text, and any type mismatch is `false`, never an error.
#[test]
fn values_compare_against_rust_scalars() {
    let doc = parse(
        r#"{"s":"passé","b":true,"i":-42,"u":18446744073709551615,"f":1.5e2,"z":-0}"#.as_bytes(),
    );
    let root = doc.value();
    let field = |key: &str| root.get(key).unwrap();

    assert_eq!(field("s"), "passé");
    assert_eq!(field("s"), *"passé");
    assert_eq!("passé", field("s"));
    assert_ne!(field("s"), "other");
    assert_eq!(field("b"), true);
    assert_eq!(true, field("b"));
    assert_eq!(field("i"), -42i64);
    assert_eq!(-42i64, field("i"));
    assert_eq!(field("u"), u64::MAX);
    assert_ne!(field("u"), -1i64, "u64::MAX is out of i64 range");
    assert_eq!(field("f"), 150.0f64);
    assert_ne!(field("f"), 150i64, "an exponent literal is not an integer");
    assert_eq!(field("z"), 0.0f64, "-0 reads as the float -0.0");
    assert_ne!(field("f"), f64::NAN);
    assert_ne!(field("b"), 1i64, "no cross-type coercion");
    assert_ne!(field("i"), "-42");

    // Owned handles compare identically, in both directions.
    let handle = doc;
    let owned = |key: &str| handle.get(key).unwrap();
    assert_eq!(owned("s"), "passé");
    assert_eq!("passé", owned("s"));
    assert_eq!(owned("i"), -42i64);
    assert_eq!(u64::MAX, owned("u"));
    assert_eq!(150.0f64, owned("f"));
    assert_eq!(owned("b"), true);
    assert_ne!(owned("i"), 0i64);
}

/// Membership tests answer `false` — never an error — off objects, and match
/// keys by their unescaped text.
#[test]
fn contains_key_checks_object_membership() {
    let doc = parse(r#"{"present":null,"esc\naped":1,"list":[1]}"#.as_bytes());
    let root = doc.value();
    assert!(root.contains_key("present"));
    assert!(root.contains_key("esc\naped"));
    assert!(!root.contains_key("absent"));
    assert!(
        !root.get("present").unwrap().contains_key("x"),
        "scalars hold no keys"
    );
    assert!(!root.get("list").unwrap().contains_key("0"));

    let handle = doc;
    assert!(handle.contains_key("present"));
    assert!(!handle.contains_key("absent"));
}

/// Owned-handle iteration borrows keys from the document: an escape-free key
/// costs no allocation, and only escaped keys unescape into owned text.
#[test]
fn object_iter_borrows_escape_free_keys() {
    use std::borrow::Cow;

    let root = parse(r#"{"plain":1,"esc\naped":2}"#.as_bytes());
    let members: Vec<(Cow<'_, str>, apollo_json::Value)> = root.object_iter().collect();
    assert!(matches!(&members[0].0, Cow::Borrowed("plain")));
    assert!(matches!(&members[1].0, Cow::Owned(key) if key == "esc\naped"));
    assert_eq!(members[0].1.as_i64(), Some(1));
    assert_eq!(members[1].1.as_i64(), Some(2));

    // The values are owned handles: they stay readable after the iteration
    // source is gone.
    let values: Vec<apollo_json::Value> = root.object_iter().map(|(_, v)| v).collect();
    drop(root);
    assert_eq!(values[1].as_i64(), Some(2));
}

/// Lookups on a wide object (past the width where the parser's duplicate
/// detection switches to a hashed index) agree with lookups on a narrow one:
/// every key resolves to its value, escaped keys match by unescaped text,
/// and misses stay misses.
#[test]
fn wide_object_lookups_resolve_every_key() {
    let members: Vec<String> = (0..100).map(|i| format!(r#""key{i}":{i}"#)).collect();
    let json = format!(r#"{{{},"esc\naped":-1,"key5":505}}"#, members.join(","));
    let doc = parse(json.as_bytes());
    let root = doc.value();

    assert_eq!(root.len(), Some(101), "the duplicate collapsed");
    for i in 0..100 {
        let expected = if i == 5 { 505 } else { i };
        assert_eq!(
            root.get(&format!("key{i}")).and_then(|v| v.as_i64()),
            Some(expected),
        );
    }
    assert_eq!(root.get("esc\naped").and_then(|v| v.as_i64()), Some(-1));
    assert!(root.get("key100").is_none());
    assert!(root.get("key").is_none());
    assert_eq!(doc.get("key99").unwrap(), 99i64);
    assert_eq!(
        root.get_path(&["key7".into()]).and_then(|v| v.as_i64()),
        Some(7)
    );
}

#[test]
fn compact_preserves_bytes() {
    let input: &[u8] = r#"{"n":1e2,"s":"passé","deep":{"list":[1,2,{"k":"v"}]}}"#.as_bytes();
    let doc = parse(input);
    assert_eq!(doc.compact().to_vec(), input);
}

/// Legacy conversions preserve content (numbers within `f64`/integer range
/// convert losslessly; formatting is `serde_json`'s).
#[test]
fn legacy_conversions_round_trip_semantically() {
    let input: &[u8] =
        r#"{"a":[1,2.5,true,null],"s":"passé","o":{"n":-7,"u":18446744073709551615}}"#.as_bytes();
    let doc = parse(input);

    let legacy = doc.to_legacy();
    let via_legacy: serde_json::Value =
        serde_json::from_slice(&serde_json::to_vec(&legacy).unwrap()).unwrap();
    let direct: serde_json::Value = serde_json::from_slice(input).unwrap();
    assert_eq!(via_legacy, direct);

    let back = Value::from_legacy(&legacy);
    let reparsed: serde_json::Value = serde_json::from_slice(&back.to_vec()).unwrap();
    assert_eq!(reparsed, direct);
}

/// Conversion keeps key insertion order in both directions.
#[test]
fn legacy_conversions_preserve_key_order() {
    let input: &[u8] = br#"{"z":1,"a":2,"m":3}"#;
    let doc = parse(input);
    let legacy = doc.to_legacy();
    assert_eq!(Value::from_legacy(&legacy).to_vec(), input);
}

/// Every serialization form emits the same bytes as `to_vec`, at chunk
/// sizes that force flushes at awkward places, on parsed documents,
/// compositions (foreign spans), and mutated documents (owned text).
#[test]
fn serialization_forms_agree_with_to_vec() {
    use apollo_json::{NewValue, PathSegment, ValueBuilder};

    let parsed = parse(r#"{"n":1e2,"s":"passé","long":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","deep":{"list":[1,2,{"k":"v"}]}}"#.as_bytes());

    let mut builder = ValueBuilder::new();
    builder
        .set_path(
            &[PathSegment::Key("adopted")],
            NewValue::Node(parsed.get("long").unwrap()),
        )
        .unwrap();
    builder
        .set_path(&[PathSegment::Key("own")], NewValue::String("plain".into()))
        .unwrap();
    let composed = builder.seal();

    for doc in [parsed, composed] {
        let expected = doc.to_vec();
        assert_eq!(doc.to_string().as_bytes(), expected);
        assert_eq!(doc.to_bytes(), expected);

        let mut written = Vec::new();
        doc.write_to(&mut written).unwrap();
        assert_eq!(written, expected);

        for chunk_size in [1, 7, 64, 4096] {
            let streamed: Vec<u8> = doc
                .clone()
                .into_chunks(chunk_size)
                .flat_map(|chunk| chunk.to_vec())
                .collect();
            assert_eq!(streamed, expected, "chunk size {chunk_size}");
        }
    }
}

/// Large clean spans stream as zero-copy slices of the arena input.
#[test]
fn streaming_yields_zero_copy_slices_for_large_spans() {
    let long = "x".repeat(5000);
    let json = format!(r#"{{"blob":"{long}","n":1}}"#);
    let doc = parse(json.as_bytes());
    let chunks: Vec<bytes::Bytes> = doc.clone().into_chunks(1024).collect();
    let flattened: Vec<u8> = chunks.iter().flat_map(|c| c.to_vec()).collect();
    assert_eq!(flattened, doc.to_vec());
    assert!(
        chunks.iter().any(|c| c.len() == 5000),
        "the blob span should surface as one zero-copy chunk"
    );
}
