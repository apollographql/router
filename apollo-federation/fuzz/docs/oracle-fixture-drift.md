# The `__typename` fixture drift

The plan lane's first campaign against `26c8a96` reported 1,114 divergences — 212 distinct
completeness operand pairs and 133 soundness ones, every one of them Lean rejecting a plan both
Rust checkers accept. **All of them were a bug in this harness, in neither checker.** They are
recorded here because the way they presented is worth not repeating.

## What was wrong

`lean/QueryPlanCheckerOracle.lean` declared its fixture's types with `id`, `f` and `g` and no
`__typename`, while the grammar generates `__typename` selections — the oracle's own digest listed
it under `selections`. graphql-lean's `Schema` does not model introspection, so
`schema.lookupField "T2" "__typename"` was `none` and every generated case containing `__typename`
compared operations that were *invalid against that schema*. `includesBool_sound` and
`includesBool_complete` both require `operationDefinitionValid`, so the oracle was running the
checker outside its contract. `docs/query-plan.md` states the precondition: a supergraph schema
given to these statements has to declare `__typename` on its entity types.

The Rust side has no such gap — apollo-compiler knows `__typename` natively — which is why it and
the legacy checker agreed with each other throughout.

The mechanism is specifically *widening across a type condition*. On the old fixture:

| pair | verdict |
| --- | --- |
| `is { __typename }` vs `is { __typename }` | true |
| `is { f }` vs `is { ... on T2 { f } }` | true |
| `is { __typename }` vs `is { ... on T2 { __typename } }` | **false** |

To see that an unconditioned selection covers a `... on T2` one the checker has to resolve the
field at `T2`, and for `__typename` there was nothing to resolve. Same shape on both sides needs
no widening and passed, which is why only a subset of cases diverged.

## The fix

`__typename` is now declared on `Query` and on every composite type in the oracle's fixture. Both
reported witnesses flip, and the sweep goes to zero:

```
cases: 12960   divergences: 0
```

## What the digest missed, and now doesn't

The `schema` request existed precisely to catch the two fixture copies drifting apart, and it did
not catch this: it compared object names, entity types, keys, requires, variables and the
selection vocabulary — but not what each type *declares*. A field missing from one side's schema
was invisible to it.

The digest now renders each type's fields as `name:NamedType`, sorted, for a fixed list of types.
The Rust side reads them from the real supergraph schema rather than from constants, so a drift
between the checked-in SDL and the oracle's hand-written tables is caught too. `__typename` is
listed explicitly on both sides, since only one of them has to write it down.

Reintroducing the bug now fails before any case runs:

```
assertion `left == right` failed: the two copies of the fixture have drifted apart
  left:  … I{ f:Int g:Int id:ID } …
  right: … I{ __typename:String f:Int g:Int id:ID } …
```

## Two lessons about reductions

Both hand reductions in the original report were wrong, in the same way: they dropped the part
that was actually doing the work.

- Shape A's reduction dropped an unguarded `is` from the left. Rust then rejected it too — under
  `¬$v0` the left really does not select `is` while the right does.
- Shape B's reduction, `is { f }` versus `is { ... on T2 { f } }`, is accepted by Lean. The
  `__typename` selections that looked like noise were the entire trigger.

A reduction is only evidence if it is checked against **both** sides, and the oracle takes grammar
bytes rather than text, so a hand-written pair cannot be. Reduce by finding a smaller *case* the
grammar generates, or verify the reduction in Lean directly.
