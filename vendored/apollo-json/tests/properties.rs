//! Property-based tests for the sharing algebra: serialization stability
//! under sharing, mutation isolation in both directions under arbitrary
//! shared topologies, retention-boundary fidelity, and agreement of builder
//! op sequences and `merge` with a `serde_json::Value` reference model.

// Each property runs dozens of generated cases; under Miri that multiplies
// interpreter overhead into hours. The paths exercised here are covered
// under Miri by the example-based suites and the reduced concurrency
// stress, so this suite is native-only.
#![cfg(not(miri))]

use apollo_json::{NewValue, PathSegment, ValueBuilder};
use proptest::prelude::*;
use serde_json::Value;

mod common;
use common::{arb_json_with, arb_text};

// --- generators -----------------------------------------------------------

/// Scalars only — the builder cannot represent u64 beyond i64 or non-finite
/// floats, so the generator stays inside the mutable value space.
fn arb_scalar() -> impl Strategy<Value = Value> {
    prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        any::<i64>().prop_map(|i| Value::Number(i.into())),
        (-1e12f64..1e12f64).prop_map(|f| serde_json::json!(f)),
        arb_text().prop_map(Value::String),
    ]
}

fn arb_value() -> impl Strategy<Value = Value> {
    arb_json_with(arb_scalar())
}

/// A document whose root is a container, so op sequences have something to
/// address.
fn arb_container() -> impl Strategy<Value = Value> {
    prop_oneof![
        prop::collection::vec(arb_value(), 0..5).prop_map(Value::Array),
        prop::collection::vec((arb_text(), arb_value()), 0..5)
            .prop_map(|kvs| Value::Object(kvs.into_iter().collect())),
    ]
}

/// Raw material for one mutation op, resolved against the current model
/// state by [`apply_ops`].
type OpSeed = (u8, u16, u16, Value);

fn arb_ops() -> impl Strategy<Value = Vec<OpSeed>> {
    prop::collection::vec(
        (any::<u8>(), any::<u16>(), any::<u16>(), arb_scalar()),
        1..10,
    )
}

// --- model helpers --------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
enum Seg {
    Key(String),
    Index(usize),
}

fn segments(path: &[Seg]) -> Vec<PathSegment<'_>> {
    path.iter()
        .map(|seg| match seg {
            Seg::Key(key) => PathSegment::Key(key),
            Seg::Index(index) => PathSegment::Index(*index),
        })
        .collect()
}

/// All paths in `value`, root first, in document order.
fn all_paths(value: &Value) -> Vec<Vec<Seg>> {
    let mut out = Vec::new();
    let mut stack = vec![(Vec::new(), value)];
    while let Some((path, value)) = stack.pop() {
        out.push(path.clone());
        match value {
            Value::Array(items) => {
                for (i, item) in items.iter().enumerate() {
                    let mut p = path.clone();
                    p.push(Seg::Index(i));
                    stack.push((p, item));
                }
            }
            Value::Object(map) => {
                for (k, member) in map {
                    let mut p = path.clone();
                    p.push(Seg::Key(k.clone()));
                    stack.push((p, member));
                }
            }
            _ => {}
        }
    }
    out
}

fn container_paths(value: &Value) -> Vec<Vec<Seg>> {
    all_paths(value)
        .into_iter()
        .filter(|path| matches!(value_at(value, path), Value::Array(_) | Value::Object(_)))
        .collect()
}

fn value_at<'a>(value: &'a Value, path: &[Seg]) -> &'a Value {
    path.iter().fold(value, |v, seg| match (v, seg) {
        (Value::Array(items), Seg::Index(i)) => &items[*i],
        (Value::Object(map), Seg::Key(k)) => &map[k],
        _ => panic!("path resolves in the model"),
    })
}

fn value_at_mut<'a>(value: &'a mut Value, path: &[Seg]) -> &'a mut Value {
    path.iter().fold(value, |v, seg| match (v, seg) {
        (Value::Array(items), Seg::Index(i)) => &mut items[*i],
        (Value::Object(map), Seg::Key(k)) => map.get_mut(k).expect("path resolves"),
        _ => panic!("path resolves in the model"),
    })
}

// The reference model's `serde_json::Value` owns the plain `Value` name here.
fn handle_at(doc: &apollo_json::Value, path: &[Seg]) -> apollo_json::Value {
    path.iter().fold(doc.clone(), |h, seg| match seg {
        Seg::Key(key) => h.get(key).expect("path resolves in the document"),
        Seg::Index(index) => h.index(*index).expect("path resolves in the document"),
    })
}

fn to_new_value(scalar: &Value) -> NewValue<'_> {
    match scalar {
        Value::Null => NewValue::Null,
        Value::Bool(b) => NewValue::Bool(*b),
        Value::Number(n) => match n.as_i64() {
            Some(i) => NewValue::Int(i),
            None => NewValue::Float(n.as_f64().expect("generated numbers are i64 or f64")),
        },
        Value::String(s) => NewValue::String(s.clone().into()),
        _ => unreachable!("op seeds carry scalars only"),
    }
}

fn parse_model(model: &Value) -> apollo_json::Value {
    apollo_json::Value::parse(serde_json::to_vec(model).expect("model serializes"))
        .expect("model JSON parses")
}

fn semantic(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes).expect("serializer output is valid JSON")
}

/// Sets `value` through a chained cursor at `path` + `last`, covering the
/// cursor localization path that `set_path` does not take.
fn cursor_set(
    builder: &mut ValueBuilder,
    path: &[Seg],
    last: PathSegment<'_>,
    value: NewValue,
) -> Result<(), apollo_json::JsonError> {
    let Some((first, rest)) = path.split_first() else {
        return builder.set(last, value);
    };
    let mut cursor = builder.get_mut(match first {
        Seg::Key(k) => PathSegment::Key(k),
        Seg::Index(i) => PathSegment::Index(*i),
    })?;
    for seg in rest {
        cursor = cursor.get_mut(match seg {
            Seg::Key(k) => PathSegment::Key(k),
            Seg::Index(i) => PathSegment::Index(*i),
        })?;
    }
    cursor.set(last, value)
}

/// Applies each op seed to the builder and, identically, to the reference
/// model. Ops are resolved against the current model state so every builder
/// call is valid by construction; the builder result must then equal the
/// model exactly.
fn apply_ops(builder: &mut ValueBuilder, model: &mut Value, ops: &[OpSeed]) {
    let mut fresh = 0usize;
    for (kind, container_sel, slot_sel, scalar) in ops {
        // Occasionally replace the whole root through the empty path.
        if *kind % 31 == 0 {
            builder
                .set_path(&[], to_new_value(scalar))
                .expect("root replacement always applies");
            *model = scalar.clone();
            continue;
        }
        let containers = container_paths(model);
        if containers.is_empty() {
            continue;
        }
        let path = containers[*container_sel as usize % containers.len()].clone();
        let via_cursor = kind & 0x40 != 0;
        match kind % 3 {
            // Set: replace-or-append an object member / array element.
            0 => {
                let last = match value_at(model, &path) {
                    Value::Object(map) => {
                        let slot = *slot_sel as usize % (map.len() + 1);
                        Seg::Key(map.keys().nth(slot).cloned().unwrap_or_else(|| {
                            fresh += 1;
                            format!("fresh-{fresh}")
                        }))
                    }
                    Value::Array(items) => Seg::Index(*slot_sel as usize % (items.len() + 1)),
                    _ => unreachable!("only container paths are selected"),
                };
                let last_segment = match &last {
                    Seg::Key(k) => PathSegment::Key(k),
                    Seg::Index(i) => PathSegment::Index(*i),
                };
                if via_cursor {
                    cursor_set(builder, &path, last_segment, to_new_value(scalar))
                        .expect("resolved set applies");
                } else {
                    let mut full = segments(&path);
                    full.push(last_segment);
                    builder
                        .set_path(&full, to_new_value(scalar))
                        .expect("resolved set applies");
                }
                match (value_at_mut(model, &path), last) {
                    (Value::Object(map), Seg::Key(key)) => {
                        map.insert(key, scalar.clone());
                    }
                    (Value::Array(items), Seg::Index(slot)) => {
                        if slot < items.len() {
                            items[slot] = scalar.clone();
                        } else {
                            items.push(scalar.clone());
                        }
                    }
                    _ => unreachable!(),
                }
            }
            // Remove: an existing member or element.
            1 => {
                let last = match value_at(model, &path) {
                    Value::Object(map) if !map.is_empty() => {
                        let slot = *slot_sel as usize % map.len();
                        Seg::Key(map.keys().nth(slot).expect("slot in range").clone())
                    }
                    Value::Array(items) if !items.is_empty() => {
                        Seg::Index(*slot_sel as usize % items.len())
                    }
                    _ => continue, // Nothing to remove from an empty container.
                };
                let mut full = segments(&path);
                full.push(match &last {
                    Seg::Key(k) => PathSegment::Key(k),
                    Seg::Index(i) => PathSegment::Index(*i),
                });
                assert!(
                    builder.remove_path(&full).expect("path is non-empty"),
                    "resolved remove must remove"
                );
                match (value_at_mut(model, &path), last) {
                    (Value::Object(map), Seg::Key(k)) => {
                        map.remove(&k);
                    }
                    (Value::Array(items), Seg::Index(i)) => {
                        items.remove(i);
                    }
                    _ => unreachable!(),
                }
            }
            // Push: arrays only; objects skip (covered by Set's append arm).
            _ => {
                if let Value::Array(items) = value_at_mut(model, &path) {
                    builder
                        .push_path(&segments(&path), to_new_value(scalar))
                        .expect("push to an existing array applies");
                    items.push(scalar.clone());
                }
            }
        }
    }
}

/// Reference deep merge with the documented semantics: object keys union
/// recursively, array elements merge index-wise (extras appended), scalars
/// and mismatched shapes replace.
fn merge_reference(target: &mut Value, source: &Value) {
    match (target, source) {
        (Value::Object(t), Value::Object(s)) => {
            for (key, sv) in s {
                match t.get_mut(key) {
                    Some(tv) => merge_reference(tv, sv),
                    None => {
                        t.insert(key.clone(), sv.clone());
                    }
                }
            }
        }
        (Value::Array(t), Value::Array(s)) => {
            for (i, sv) in s.iter().enumerate() {
                if i < t.len() {
                    merge_reference(&mut t[i], sv);
                } else {
                    t.push(sv.clone());
                }
            }
        }
        (t, s) => *t = s.clone(),
    }
}

// --- properties -----------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// parse ∘ serialize is a fixpoint: the first serialization reparses,
    /// and reserializes byte-identically; both agree semantically with the
    /// generating model.
    #[test]
    fn parse_serialize_is_idempotent(model in arb_value()) {
        let doc = parse_model(&model);
        let first = doc.to_vec();
        prop_assert_eq!(&semantic(&first), &model);
        let second = apollo_json::Value::parse(first.clone()).expect("own output reparses").to_vec();
        prop_assert_eq!(first, second);
    }

    /// Sharing is serialization-invariant: any fragment serializes to the
    /// same bytes as a handle, as the root of a sealed document, and as an
    /// adopted member of a composition — and sharing never disturbs the
    /// source document.
    #[test]
    fn serialize_of_shared_fragment_equals_serialize(
        model in arb_value(),
        path_sel in any::<u16>(),
    ) {
        let doc = parse_model(&model);
        let source_bytes = doc.to_vec();
        let paths = all_paths(&model);
        let path = &paths[path_sel as usize % paths.len()];
        let handle = handle_at(&doc, path);
        let fragment_bytes = handle.to_vec();

        // As the root of a fresh document.
        let mut builder = ValueBuilder::new();
        builder.set_path(&[], NewValue::Node(handle.clone())).expect("root always sets");
        prop_assert_eq!(builder.seal().to_vec(), fragment_bytes.clone());

        // As an adopted member of a composition, twice (DAG expansion).
        let mut builder = ValueBuilder::new();
        builder.set("first", handle.clone()).expect("root is an object");
        builder.set("second", handle).expect("root is an object");
        let composed = builder.seal();
        let composed_root = composed.value();
        for member in ["first", "second"] {
            let got = composed_root.get(member).expect("member exists").to_vec();
            prop_assert_eq!(&got, &fragment_bytes);
        }

        prop_assert_eq!(doc.to_vec(), source_bytes);
    }

    /// An op sequence applied through the builder equals the same sequence
    /// applied to the reference model — and never leaks into the shared
    /// original (isolation direction one); a second edit of the original
    /// afterwards never leaks into the first result (direction two).
    #[test]
    fn op_sequences_agree_with_model_and_isolate(
        model in arb_container(),
        ops in arb_ops(),
        second_ops in arb_ops(),
    ) {
        let original = parse_model(&model);
        let original_bytes = original.to_vec();

        let mut expected = model.clone();
        let mut builder = original.clone().edit();
        apply_ops(&mut builder, &mut expected, &ops);
        let edited = builder.seal();
        let edited_bytes = edited.to_vec();

        prop_assert_eq!(&semantic(&edited_bytes), &expected);
        prop_assert_eq!(original.to_vec(), original_bytes.clone());

        // Direction two: mutate the original while `edited` is live.
        let mut expected_second = model.clone();
        let mut builder = original.clone().edit();
        apply_ops(&mut builder, &mut expected_second, &second_ops);
        let second = builder.seal();

        prop_assert_eq!(&semantic(&second.to_vec()), &expected_second);
        prop_assert_eq!(edited.to_vec(), edited_bytes);
        prop_assert_eq!(original.to_vec(), original_bytes);
    }

    /// Mutating a composition never disturbs the documents it adopted from,
    /// and mutating a source afterwards never disturbs the composition.
    #[test]
    fn shared_topologies_isolate_in_both_directions(
        models in prop::collection::vec(arb_value(), 3),
        path_sels in prop::collection::vec(any::<u16>(), 3),
        ops in arb_ops(),
    ) {
        let sources: Vec<apollo_json::Value> = models.iter().map(parse_model).collect();
        let source_bytes: Vec<Vec<u8>> = sources.iter().map(apollo_json::Value::to_vec).collect();

        // Compose one fragment of each source, then mutate the composition.
        let mut composition_model = Value::Object(serde_json::Map::new());
        let mut builder = ValueBuilder::new();
        for (i, (model, sel)) in models.iter().zip(&path_sels).enumerate() {
            let paths = all_paths(model);
            let path = &paths[*sel as usize % paths.len()];
            builder
                .set(format!("frag{i}").as_str(), handle_at(&sources[i], path))
                .expect("root is an object");
            composition_model[format!("frag{i}")] = value_at(model, path).clone();
        }
        apply_ops(&mut builder, &mut composition_model, &ops);
        let composed = builder.seal();
        let composed_bytes = composed.to_vec();
        prop_assert_eq!(&semantic(&composed_bytes), &composition_model);
        for (source, bytes) in sources.iter().zip(&source_bytes) {
            prop_assert_eq!(&source.to_vec(), bytes, "sources must not observe composition edits");
        }

        // Now mutate each source; the composition must not move.
        for (i, source) in sources.iter().enumerate() {
            let mut expected = models[i].clone();
            let mut builder = source.clone().edit();
            apply_ops(&mut builder, &mut expected, &ops);
            let mutated = builder.seal();
            prop_assert_eq!(&semantic(&mutated.to_vec()), &expected);
        }
        prop_assert_eq!(composed.to_vec(), composed_bytes);
    }

    /// `merge` agrees with the reference deep-merge model and leaves both
    /// the target's shared original and the merged-in source untouched.
    #[test]
    fn merge_agrees_with_reference_model(
        target_model in arb_value(),
        source_model in arb_value(),
    ) {
        let target = parse_model(&target_model);
        let source = parse_model(&source_model);
        let target_bytes = target.to_vec();
        let source_bytes = source.to_vec();

        let mut expected = target_model.clone();
        merge_reference(&mut expected, &source_model);

        let mut builder = target.clone().edit();
        builder.merge(&source);
        let merged = builder.seal();

        prop_assert_eq!(&semantic(&merged.to_vec()), &expected);
        prop_assert_eq!(target.to_vec(), target_bytes);
        prop_assert_eq!(source.to_vec(), source_bytes);
    }

    /// The retention boundary: `into_self_contained` and `detach` preserve
    /// bytes exactly and the results depend on no source arena.
    #[test]
    fn retention_boundary_preserves_bytes_and_severs_pins(
        models in prop::collection::vec(arb_value(), 3),
        path_sels in prop::collection::vec(any::<u16>(), 3),
    ) {
        let sources: Vec<apollo_json::Value> = models.iter().map(parse_model).collect();

        let mut builder = ValueBuilder::new();
        let mut fragment_bytes = Vec::new();
        for (i, (model, sel)) in models.iter().zip(&path_sels).enumerate() {
            let paths = all_paths(model);
            let path = &paths[*sel as usize % paths.len()];
            let handle = handle_at(&sources[i], path);
            fragment_bytes.push(handle.to_vec());
            builder
                .set(format!("frag{i}").as_str(), handle.clone())
                .expect("root is an object");

            // detach: equal bytes, independent of the source arena.
            let detached = handle.compact();
            prop_assert!(detached.is_self_contained());
            prop_assert_eq!(detached.to_vec(), fragment_bytes[i].clone());
        }
        let composed = builder.seal();
        let expected = composed.to_vec();
        prop_assert!(!composed.is_self_contained());

        let retained = composed.into_self_contained();
        drop(sources);
        prop_assert!(retained.is_self_contained());
        prop_assert_eq!(retained.to_vec(), expected);
    }
}
