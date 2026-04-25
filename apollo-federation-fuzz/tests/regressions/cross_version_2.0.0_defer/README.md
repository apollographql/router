# `@defer` sweep — two new divergence classes surfaced

This run was the first defer-enabled sweep. Required a one-line fix in
`op_gen.rs` first: composed federation supergraphs don't declare the
`@defer` directive, so apollo-compiler validation rejected ~50% of
decorated ops. With the directive declaration injected for validation,
ops_skipped drops from 527 to 0.

## Sweep results

    cargo run -p apollo-federation-fuzz --bin fuzz --release -- \
        --iterations 1000 --seed 17 --ops-per-schema 5 --enable-defer \
        --regressions-dir tests/regressions/cross_version_2.0.0_defer

| BASE -> 2.13.1 + `--enable-defer` | Iter / op | Divergences |
|------------------------------------|-----------|-------------|
| `=2.0.0`                          | 1000      | **554**     |
| `=2.1.3`                          | 1000      | 201         |
| `=2.5.0` (4 seeds × 1000 ops)     | 4 × 1000  | 0           |
| `=2.13.0`                         | 1000      | 0           |

554 divergences against 2.0.0 — a 6× jump over the same surface
without defer (87). Fix window remains (2.1.3, 2.5.0].

## Two new divergence classes

Refined categorization of the 554 divergences (categories overlap):

| Pattern | Files |
|---|---|
| Contains PR #7580 (`... on Query`) | 201 |
| Contains FED-505 / Condition node difference | 389 |
| Contains **C: `Defer.primary.sub_selection` added in HEAD** | 522 |
| Contains **D: `query_path[].Field` representation change** | 438 |

### Class C: `Defer.primary.sub_selection` field added

HEAD includes a `sub_selection` string on each `Defer.primary` node
that summarizes the immediate (non-deferred) selection. BASE doesn't
emit this field at all.

```diff
 "Defer": {
   "primary": {
+    "sub_selection": "{ qT0 @include(if: $__v1) { ... } qT0 { ... } }",
     "node": { "Fetch": { ... } }
   },
```

Likely a deliberate executor-coordination addition rather than a bug
fix; documenting it as wire-format drift.

### Class D: `query_path[].Field` representation

BASE encodes `query_path` field elements as raw strings *with directive
text baked in*. HEAD uses a structured `{response_key: "..."}` and
omits runtime directives.

```diff
 "query_path": [
   {
-    "Field": "qI0 @skip(if: $__v0)"
+    "Field": {
+      "response_key": "qI0"
+    }
   }
 ]
```

This looks like a **real semantic fix**: directives like
`@skip(if: $v)` are runtime-conditional, so embedding them in a static
query-path element couples plan structure to variable values, which is
incorrect for things like merging deferred responses against the
primary stream. HEAD's structured form keeps the path purely
identifying.

## Curated artifact

- `defer_subselection_and_fieldrepr.txt` — a "pure" defer divergence:
  no PR #7580, no Condition node difference, just the two defer-
  specific classes C and D side by side.

## Conclusion

The `@defer` flag was wired through the harness from earlier work but
had never been exercised in a sweep. This sweep:

1. Closes COVERAGE_GAPS row 8 (`@defer`) with empirical evidence.
2. Surfaces two new divergence classes (C and D) that complement the
   existing PR #7580 and FED-505 findings.
3. Confirms the same fix window — (2.1.3, 2.5.0] — applies to defer
   plan format/semantic improvements as to the non-defer fixes.

Class D in particular is the kind of finding that would be harder to
spot manually: a string-vs-struct representation difference inside a
plan structure that's only emitted when `@defer` is in the operation.

## Reproducing

```toml
# apollo-federation-fuzz/Cargo.toml
apollo-federation-base = { package = "apollo-federation", version = "=2.0.0" }
```

```sh
cargo run -p apollo-federation-fuzz --bin fuzz --release -- \
    --iterations 1000 --seed 17 --ops-per-schema 5 --enable-defer \
    --regressions-dir tests/regressions/cross_version_2.0.0_defer
```

Default `Cargo.toml` is back at `=2.13.0` so `cargo test` is clean.
