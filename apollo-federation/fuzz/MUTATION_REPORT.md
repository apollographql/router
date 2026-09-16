# Mutation report — `query_compare`

What fraction of seeded defects do the tests catch? Snapshot of 2026-09-16.

```sh
cargo mutants --in-place -f 'apollo-federation/src/correctness/query_compare/**' \
  --package apollo-federation -- --lib correctness::query_compare
```

`--in-place` edits the real files for the duration, so nothing else may build against
`apollo-federation` while it runs — results taken during a campaign are reading a mutated checker.
It also forbids `--jobs`, so the run is serial: 206 mutants took about an hour.

## Result

| | count |
| --- | --- |
| caught | 113 |
| **missed** | **64** |
| unviable (does not compile) | 29 |
| timeout | 0 |

**63.8% kill rate over 177 viable mutants**, with the crate's 15 unit tests as the kill signal.

These numbers describe the checker as committed. Enriching the *generator* cannot move them — the
kill signal here is the unit tests, which the harness does not touch. What a richer grammar changes
is which survivors the **lanes** can kill, which is measured by hand below.

This measures the *unit tests*, not the fuzz lanes. cargo-mutants cannot reach outside the
workspace of the package it runs in, and this package is a separate workspace, so `src/properties.rs`
is invisible to a campaign rooted at the router workspace. See "Still open" in PRIOR_ART.md.

## Where the survivors live

Every survivor, by enclosing function.

### Optimization-only — equivalent mutants (~35)

`group_locally_includes`, `scalar_field_includes*`, `composite_field_includes*`,
`selection_syntactically_includes`, `selection_set_syntactically_includes`, `same_resolver_call`,
`same_directive_list`, `boolean_condition_covered_by`, `subtract_boolean_condition`,
`same_selection_refs`, and the de-duplication comparisons in `child_tasks_for_parent_types`.

These are witnesses, not obligations. A mutation that makes a shortcut *fail* changes nothing
observable: the general search runs anyway and reaches the same verdict. No test that only observes
the verdict can kill them, whatever the generator.

The split is visible in the data and is the strongest evidence the suite is working where it
matters: for all three shortcut entry points, the `-> true` mutant was **caught** and the `-> false`
mutant **missed**.

```
caught   replace group_locally_includes -> bool with true
missed   replace group_locally_includes -> bool with false
```

`-> true` makes the checker unsound — it accepts everything — and is caught. `-> false` only
removes a fast path.

### Rendering-only (3)

`Display for BooleanLiteral`, `Display for Assignment`, `ComparisonError::path`. No verdict effect;
covered by the snapshot tests in the crate, not by anything here.

### General path (~26)

`same_argument_value` (9), `BooleanLiteral::ordered_before_or_equal` (6),
`compare_shared_variable_declarations` (3), `literals_for_directive` (3), `insert_literal` (2),
`same_arguments` (1), `SchemaView::is_composite` (1), `SchemaView::lookup_field` (1).

These are real gaps in the unit tests.

## Do the lanes kill them?

Six survivors applied by hand, then `cargo test` in this package (all five properties, no oracle):

| survivor | lanes |
| --- | --- |
| `mod.rs:191` delete match arm `(None, None)` in `compare_shared_variable_declarations` | **killed** |
| `mod.rs:842` `same_argument_value -> false` | **killed** |
| `mod.rs:827` `==` → `!=` in `same_resolver_call` | survived |
| `mod.rs:679` `scalar_field_includes -> false` | survived |
| `mod.rs:443` `==` → `!=` in `child_tasks_for_parent_types` | survived |
| `conditions.rs:144` `boolean_condition_covered_by -> false` | survived |

Both general-path survivors were killed; all four optimization-only survivors survived, as the
categorization predicts. The two killed are instructive:

- the `(None, None)` arm makes two declarations with no default compare as mismatched. No unit test
  covers it. Declaration variant 0 is `Boolean!` with no default, so the differential lane hits that
  arm thousands of times per campaign;
- `same_argument_value -> false` makes no two arguments ever compare equal. It survived the unit
  tests because every operation in the passing ones selects argument-free fields, so the comparison
  is never reached. The lanes generate `tag(n:…, label:…)` constantly, and reflexivity fails
  immediately.

This is a sample of six, not a measurement of all 64. The honest reading: the lanes strictly
improve on the unit tests where it counts, and the residue is dominated by mutants nothing can kill.

## Follow-up: closing the argument gap

The `same_argument_value` cluster (9 survivors, the largest) was traced to the schema: the only
argument-bearing field was `tag(n: Int!, label: String)`, so the `Object` and `List` arms — the two
that encode the *interesting* semantics — could never execute. The schema now has
`tag(n: Int!, label: String, flag: Boolean, tags: [String], meta: Meta, kind: Kind)`.

That alone did not make them killable, and the reasons are worth recording.

**The `List` arm is a genuine equivalent mutant.** Deleting it falls through to
`_ => left == right`, and apollo-compiler's `PartialEq` for `Value::List` already compares
element-wise in order — exactly what the arm does. No input can distinguish them.

**The `Object` arm needed three changes, not one.** It is order-*insensitive* where `==` is
order-sensitive, so telling them apart requires two operations that differ *only* in input-object
field order. Random generation practically never produces that. It took:

1. the schema change, so an input-object argument exists at all;
2. a `permute-object-argument` rewrite, which swaps the field order of an input-object argument on
   one side — semantics-preserving, so the verdict must not change. This generates the
   distinguishing shape by construction instead of waiting for it;
3. *related* input pairs in the metamorphic lane. It had been generating plain random bytes, so the
   two operations rarely agreed and a rewrite applied to an already-failing pair changed nothing.
   The lane now repeats the bytes the left operation consumed, as the differential lane does.

With all three, deleting the `Object` arm is killed by the metamorphic lane, and every violation
report names `permute-object-argument`.

This is the closed-generator lesson in miniature: the schema gap was necessary to fix but not
sufficient, and the part that actually caught the defect was an *open* edit that manufactures the
shape rather than a richer random space that might stumble on it.

## Follow-up: variables in argument position

`tag(n: 1)`, `tag(n: $i0)` and `tag(n: $i1)` are three different resolver calls, and every argument
in the grammar had been a literal. The grammar now draws `n` from `{literal, $i0, $i1}` and `label`
from `{literal, $s0}`, with `$i0`/`$i1` declared `Int!` (three variants) and `$s0` declared `String`
(two).

This does not unlock a dedicated match arm the way the input-object work did — `Value::Variable`
falls to `_ => left == right`, which compares names. What it adds is a value kind flowing through
argument comparison, and an interaction the grammar could not previously produce: two operations
that make the *same* call through the *same* variable while declaring that variable differently.

Two notes for anyone extending the Lean adapter:

- `SelectionConditions.selectionSetBooleanVariables` recovers `@skip`/`@include` variables only.
  Argument variables need their own walk, added as `selectionSetArgumentVariables`. Both sides must
  agree on the used set exactly, because an operation may only declare variables it uses;
- `meta` and `variable` are both Lean keywords. Fields and binders named after them do not parse.

## What would move the number

1. Make the lanes the kill signal, so the rate reflects them rather than the unit tests.
2. Mark the shortcut functions as `#[mutants::skip]`, so the rate stops being diluted by ~35
   mutants that are unkillable by construction. Excluding them and the rendering-only three, the
   unit-test rate over the remaining viable mutants is what actually wants driving up.
