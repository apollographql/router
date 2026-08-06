### Fold an array into a single value with `->reduce` in Connect v0.5

Connect spec v0.5 (preview, behind `connectors.preview_connect_v0_5: true`) adds `array->reduce($acc, <seed>, <update>)` to the mapping language, for the aggregations that previously had no expression at all:

```graphql
type Query {
  order: Order @connect(
    source: "api"
    http: { GET: "/order" }
    selection: """
    id
    total: lineItems->reduce($acc, 0, @.price->add($acc))
    """
  )
}
```

The first argument names the accumulator. The second is its starting value, evaluated once before the fold begins. The third runs once per element, with `@` bound to that element and the accumulator bound to the value so far; each result becomes the next accumulator, and the last one is the answer.

The starting value is also the result for an empty array, so a fold always produces a value rather than failing on empty input. Because it is written explicitly, the accumulator can be a different type than the elements.

The accumulator is visible only inside the update expression — unlike `->as`, it does not remain bound after the method, so nested folds cannot interfere with one another. For that reason `reduce`, like `as`, cannot be redefined by a `methods:` entry: the mapping language decides what the name binds before it can know which definition it resolves to.

**Mind the argument order.** `@->add($acc)` is correct; `$acc->add(@)` is not, and it fails quietly. Leading a path with something other than `@` moves `@` onto that value, so `$acc->add(@)` adds the accumulator to itself and never reads the array.

Composition now warns about exactly that mistake, wherever `@` is written in a per-element argument to `->map`, `->filter`, `->find`, or `->reduce` but cannot reach the element. It catches the same error outside folds — `items->filter($args.ids->contains(@))` asks whether a list contains itself, and filters nothing — and names the fix. An argument that never mentions `@` is left alone, since ignoring the element can be deliberate.

By [@benjamn](https://github.com/benjamn) in https://github.com/apollographql/router/pull/9969
