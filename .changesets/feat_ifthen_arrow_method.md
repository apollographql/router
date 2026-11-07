### Add `->ifThen` method for conditional mappings without `->match`

Connector mapping expressions can now branch on a boolean with
`condition->ifThen(then_expr[, else_expr])`. The then branch is taken only when
the condition is `true`; otherwise the else branch is used, or nothing is
produced when it is omitted.

```graphql
status: age->gte(18)->ifThen("adult", "minor")
discount: isPremium->ifThen(20, 0)
```

Only the taken branch is evaluated, so the untaken one cannot contribute errors:
`isValid->ifThen("then", $.missing)` yields `"then"` without complaining about
`$.missing`. This is what makes `->ifThen` a method rather than syntax sugar.

Omitting the else branch produces no value, which at a named position drops the
key and under a spread contributes nothing:

```graphql
before ...condition->ifThen({ optional: "value" }) after
```

When `condition` is `false`, the result is just `before` and `after`, with no
error. Previously an inline spread that produced no value always reported
`Inlined path produced no value`; that error is now raised only when the path
itself errored.

Conditions must be boolean, consistent with `->and`, `->or`, `->not`, `->filter`
and `->find`. There is no truthiness: `true` is the only value that takes the
then branch, so `1` and `"false"` both take the else branch. Anything that is
neither `true` nor `false` reports `Method ->ifThen can only be applied to
boolean values.` Unlike the other boolean methods the error is non-fatal:
because `->ifThen` has a fallback, it still produces a value rather than
collapsing the selection.

For two-way dispatch this replaces `->match`'s `[candidate, value]` pairs and
`[@, _]` catch-all with the branches written directly:

```graphql
# before
... type->match(["book", { __typename: "Book", title: title }],
                [@,      { __typename: "Movie", title: title }])
# after
... type->eq("book")->ifThen({ __typename: "Book", title: title },
                             { __typename: "Movie", title: title })
```

`->match` remains the better fit for dispatch over more than two cases.

By [@benjamn](https://github.com/benjamn) in https://github.com/apollographql/router/pull/9970
