//! Adversarial inputs: depth and arena caps, malformed documents, and the
//! behavior of deep documents in every iterative path.

use apollo_json::{JsonError, ParseOptions, Value};

fn nested_arrays(depth: usize) -> Vec<u8> {
    let mut json = Vec::with_capacity(depth * 2 + 1);
    json.resize(depth, b'[');
    json.push(b'1');
    json.resize(depth * 2 + 1, b']');
    json
}

#[test]
fn depth_at_the_cap_parses_and_over_it_fails() {
    assert!(Value::parse(nested_arrays(128)).is_ok());
    assert!(matches!(
        Value::parse(nested_arrays(129)),
        Err(JsonError::DepthLimitExceeded { limit: 128 })
    ));

    let options = ParseOptions::default().with_max_depth(16);
    assert!(Value::parse_with_options(nested_arrays(16), &options).is_ok());
    assert!(matches!(
        Value::parse_with_options(nested_arrays(17), &options),
        Err(JsonError::DepthLimitExceeded { limit: 16 })
    ));
}

/// The classic stack-overflow input fails fast with a depth error rather
/// than exhausting the thread stack.
#[test]
fn pathological_nesting_is_rejected_not_overflowed() {
    let mut json = vec![b'['; 100_000];
    json.push(b'1');
    assert!(matches!(
        Value::parse(json),
        Err(JsonError::DepthLimitExceeded { .. })
    ));
}

/// A document at the depth cap flows through every iterative path:
/// serialize (buffered and streaming), detach, and both legacy conversions.
#[test]
fn deep_document_survives_all_walks() {
    let input = nested_arrays(128);
    let doc = Value::parse(input.clone()).unwrap();
    assert_eq!(doc.to_vec(), input);
    let streamed: Vec<u8> = doc
        .clone()
        .into_chunks(16)
        .flat_map(|chunk| chunk.to_vec())
        .collect();
    assert_eq!(streamed, input);
    assert_eq!(doc.compact().to_vec(), input);
    let legacy = doc.to_legacy();
    assert_eq!(Value::from_legacy(&legacy).to_vec(), input);
}

/// The depth cap counts object frames and mixed nesting the same way it
/// counts arrays.
#[test]
fn deep_objects_and_mixed_nesting_hit_the_same_cap() {
    fn nested_objects(depth: usize) -> Vec<u8> {
        let mut json = br#"{"k":"#.repeat(depth);
        json.push(b'1');
        json.extend(std::iter::repeat_n(b'}', depth));
        json
    }
    fn mixed(depth: usize) -> Vec<u8> {
        let mut json = Vec::new();
        for level in 0..depth {
            json.extend_from_slice(if level % 2 == 0 { b"[" } else { br#"{"k":"# });
        }
        json.push(b'1');
        for level in (0..depth).rev() {
            json.push(if level % 2 == 0 { b']' } else { b'}' });
        }
        json
    }
    for build in [nested_objects, mixed] {
        let at_cap = build(128);
        let doc = Value::parse(at_cap.clone()).unwrap();
        assert_eq!(doc.to_vec(), at_cap);
        assert!(matches!(
            Value::parse(build(129)),
            Err(JsonError::DepthLimitExceeded { limit: 128 })
        ));
    }
}

/// A long chain of documents, each referencing the previous one's arena,
/// drops without recursing through nested arena destructors.
#[test]
fn long_cross_arena_chains_drop_iteratively() {
    let depth = if cfg!(miri) { 64 } else { 50_000 };
    let mut doc = Value::parse(br#"{"leaf":1}"#.to_vec()).unwrap();
    for _ in 0..depth {
        let mut builder = apollo_json::ValueBuilder::new();
        builder.set("next", doc).expect("root is an object");
        doc = builder.seal();
    }
    // Dropping the head releases every arena in the chain; a recursive drop
    // would overflow the stack long before 50k levels.
    drop(doc);
}

#[test]
fn arena_cap_aborts_the_parse() {
    let options = ParseOptions::default().with_max_arena_bytes(1024);
    let big = format!(r#"{{"items":[{}]}}"#, vec!["1"; 2000].join(","));
    assert!(matches!(
        Value::parse_with_options(big.into_bytes(), &options),
        Err(JsonError::ArenaLimitExceeded { .. })
    ));
    // Input alone larger than the cap fails before parsing starts.
    assert!(matches!(
        Value::parse_with_options(vec![b' '; 2048], &options),
        Err(JsonError::ArenaLimitExceeded { .. })
    ));
}

/// The cap also covers entry-slab growth: a wide object (past the point
/// where duplicate detection switches to its hashed index) aborts the same
/// way an element-heavy array does.
#[test]
fn arena_cap_aborts_wide_object_parses() {
    let options = ParseOptions::default().with_max_arena_bytes(2048);
    let members: Vec<String> = (0..400).map(|i| format!(r#""key{i}":{i}"#)).collect();
    let wide = format!("{{{}}}", members.join(","));
    assert!(matches!(
        Value::parse_with_options(wide.into_bytes(), &options),
        Err(JsonError::ArenaLimitExceeded { .. })
    ));
}

#[test]
fn malformed_documents_are_rejected() {
    let cases: &[&[u8]] = &[
        b"",
        b"  ",
        b"{",
        b"[1,]",
        b"{\"a\":1,}",
        b"{\"a\"}",
        b"{\"a\":}",
        b"[1 2]",
        b"01",
        b"-",
        b"0.",
        b"1e",
        b"1e+",
        b".5",
        b"+1",
        b"tru",
        b"nul",
        b"\"unterminated",
        b"\"tab\tinside\"",
        b"\"bad \\q escape\"",
        b"\"lone \\ud800 surrogate\"",
        b"\"swapped \\ude00\\ud83d\"",
        b"[1]]",
        b"{} {}",
        b"\xef\xbb\xbf{}",
        b"\"\xff\xfe\"",
    ];
    for case in cases {
        assert!(
            matches!(Value::parse(case.to_vec()), Err(JsonError::Syntax { .. })),
            "expected rejection of {:?}",
            String::from_utf8_lossy(case)
        );
    }
}

#[test]
fn valid_documents_are_accepted() {
    let cases: &[&[u8]] = &[
        b"null",
        b"true",
        b"-0",
        b"1e-005",
        b"\"lonely string\"",
        b" [ 1 , 2 ] ",
        "{\"pair\":\"\u{1F600}\"}".as_bytes(),
        b"[0.5e3, 1E+2, 123456789012345678901234567890]",
    ];
    for case in cases {
        assert!(
            Value::parse(case.to_vec()).is_ok(),
            "expected acceptance of {:?}",
            String::from_utf8_lossy(case)
        );
    }
}

/// Long whitespace and digit runs exercise the word-at-a-time skip loops,
/// including stop positions at every lane of an eight-byte word.
#[test]
fn long_runs_of_whitespace_and_digits_parse() {
    for ws in [1usize, 7, 8, 9, 23, 40] {
        for digits in [1usize, 7, 8, 9, 17, 33] {
            let number = "9".repeat(digits);
            let pad = " \t\n\r ".repeat(ws);
            let json = format!("{pad}[{pad}{number}{pad},{pad}1{pad}]{pad}");
            let doc = Value::parse(json.into_bytes())
                .unwrap_or_else(|e| panic!("ws={ws} digits={digits}: {e}"));
            assert_eq!(doc.to_vec(), format!("[{number},1]").as_bytes());
        }
    }
}

/// Duplicate keys collapse the way `serde_json`'s `preserve_order` map does:
/// first position, first spelling, last value.
#[test]
fn duplicate_keys_keep_first_position_and_last_value() {
    let doc = Value::parse(br#"{"a":1,"b":2,"a":3}"#.to_vec()).unwrap();
    assert_eq!(doc.to_vec(), br#"{"a":3,"b":2}"#);

    // Keys match by logical text, not spelling: `\u0061` is `a`.
    let doc = Value::parse(br#"{"a":1,"\u0061":2}"#.to_vec()).unwrap();
    assert_eq!(doc.to_vec(), br#"{"a":2}"#);
}

/// Wide objects switch duplicate detection to a hashed index; collapse
/// semantics (first position, last value, logical-text matching) must not
/// change across that switch.
#[test]
fn duplicate_keys_collapse_identically_in_wide_objects() {
    let mut members: Vec<String> = (0..40).map(|i| format!(r#""key{i}":{i}"#)).collect();
    // Duplicates landing after the index is built: plain, an escaped
    // spelling of an existing key, and a repeated new key.
    members.push(r#""key5":500"#.to_owned());
    members.push("\"\\u006bey7\":700".to_owned()); // logical text `key7`
    members.push(r#""late":1"#.to_owned());
    members.push(r#""late":2"#.to_owned());
    let doc = Value::parse(format!("{{{}}}", members.join(",")).into_bytes()).unwrap();

    let mut expected: Vec<String> = (0..40)
        .map(|i| match i {
            5 => r#""key5":500"#.to_owned(),
            7 => r#""key7":700"#.to_owned(),
            i => format!(r#""key{i}":{i}"#),
        })
        .collect();
    expected.push(r#""late":2"#.to_owned());
    assert_eq!(
        doc.to_vec(),
        format!("{{{}}}", expected.join(",")).into_bytes()
    );
}

/// Mutation operates on the collapsed object: after a duplicate-key parse
/// there is exactly one member to remove or edit.
#[test]
fn duplicate_keys_collapse_before_mutation() {
    let doc = Value::parse(br#"{"a":1,"a":2,"b":3}"#.to_vec()).unwrap();
    let mut builder = doc.edit();
    assert!(builder.remove("a"));
    assert!(!builder.remove("a"), "only the collapsed member exists");
    assert_eq!(builder.seal().to_vec(), br#"{"b":3}"#);

    let doc = Value::parse(br#"{"a":{"x":1},"a":{"y":2}}"#.to_vec()).unwrap();
    let mut builder = doc.edit();
    let mut cursor = builder.get_mut("a").unwrap();
    cursor.set("z", 3).unwrap();
    assert_eq!(builder.seal().to_vec(), br#"{"a":{"y":2,"z":3}}"#);
}

/// Escape sequences truncated by the end of the input fail cleanly at every
/// possible cut point.
#[test]
fn truncated_escapes_are_rejected() {
    let cases: &[&[u8]] = &[
        b"\"abc\\",
        b"\"\\u",
        b"\"\\u1",
        b"\"\\u12",
        b"\"\\u123",
        b"\"\\ud83d",
        b"\"\\ud83d\\",
        b"\"\\ud83d\\u",
        b"\"\\ud83d\\ude0",
        b"\"\\ud83d\"\"",
    ];
    for case in cases {
        assert!(
            matches!(Value::parse(case.to_vec()), Err(JsonError::Syntax { .. })),
            "expected rejection of {:?}",
            String::from_utf8_lossy(case)
        );
    }
}

/// Invalid UTF-8 is rejected wherever it sits, including a multi-byte
/// sequence truncated by the end of the input.
#[test]
fn invalid_utf8_is_rejected_everywhere() {
    let truncated_two_byte: &[u8] = &[b'"', 0xC3]; // first byte of a two-byte char, then EOF
    let truncated_four_byte: &[u8] = &[b'"', 0xF0, 0x9F, 0x98]; // four-byte char minus its last byte
    let lone_continuation: &[u8] = &[b'"', 0x80, b'"'];
    let overlong_nul: &[u8] = &[b'"', 0xC0, 0x80, b'"'];
    let surrogate_bytes: &[u8] = &[b'"', 0xED, 0xA0, 0x80, b'"']; // UTF-8-encoded U+D800
    let outside_string: &[u8] = b"[1,\xFF]";
    for case in [
        truncated_two_byte,
        truncated_four_byte,
        lone_continuation,
        overlong_nul,
        surrogate_bytes,
        outside_string,
    ] {
        assert!(
            matches!(Value::parse(case.to_vec()), Err(JsonError::Syntax { .. })),
            "expected rejection of {case:?}"
        );
    }
}

/// Number literals beyond `f64` range or precision pass through verbatim
/// and convert to the saturating/rounded legacy value without panicking.
#[test]
fn extreme_number_literals_survive_every_path() {
    let input: &[u8] = br#"[1e999999,-1e999999,1e-999999,9007199254740993,3.14159265358979323846264338327950288419716939937510582097494459]"#;
    let doc = Value::parse(input.to_vec()).unwrap();
    assert_eq!(doc.to_vec(), input, "literals pass through verbatim");
    assert_eq!(doc.compact().to_vec(), input);

    let legacy = doc.to_legacy();
    let items = legacy.as_array().expect("array converts");
    assert_eq!(items[0].as_f64(), Some(f64::MAX), "overflow saturates");
    assert_eq!(items[1].as_f64(), Some(f64::MIN));
    assert_eq!(items[2].as_f64(), Some(0.0), "underflow rounds to zero");
    assert_eq!(items[3].as_u64(), Some(9007199254740993));
    // Round-tripping the saturated values must not panic either.
    Value::from_legacy(&legacy);
}
