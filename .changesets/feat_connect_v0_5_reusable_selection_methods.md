### Reusable custom `->` methods for Connect v0.5: `@source(methods:)`, `@connect(methods:)`, and `@method`

Connect spec v0.5 (preview, behind `connectors.preview_connect_v0_5: true`) lets you name a mapping once and call it from many selections, as a custom `->` method.

Declare methods on `@source`, on `@connect`, or derive one from a type with `@method`:

```graphql
extend schema
  @link(url: "https://specs.apollo.dev/connect/v0.5", import: ["@connect", "@source", "@method"])
  @source(
    name: "api"
    http: { baseURL: "https://api.example.com" }
    methods: { clamp: "($lo, $hi) => @->min($hi)->max($lo)" }
  )

type User @method {
  id: ID!
  name: String
  email: String
}

type Query {
  user: User @connect(source: "api", http: { GET: "/user" }, selection: "$->User")
  team: [User] @connect(source: "api", http: { GET: "/team" }, selection: "$->map(@->User)")
}
```

A method body refers to its own input as `@`. `$` is left alone, so it still means what it would mean where the call appears.

Methods share **one namespace per subgraph**, whichever directive declares them. Putting a method on a `@connect` keeps it next to the connector that motivated it; it does *not* scope it to that connector, and it still collides with a same-named method declared anywhere else in the subgraph.

A method may reuse the name of a built-in `->method`, in which case yours takes precedence and composition emits a warning. This keeps new built-in methods purely additive: a schema that already defines the name keeps the behavior it wrote and tested, rather than silently changing meaning or failing to compose when a built-in of that name ships. The exception is `->as`, whose meaning is fixed by the mapping language itself — it decides which names become bound variables, at parse time — and so cannot be redefined.

Calling a `->method` that is neither a built-in nor a declared method is now reported at composition instead of only failing per request. On connect v0.5 this is an error; on v0.1–v0.4, which shipped without the check, it is a warning, so schemas that deploy today keep composing.

Methods do not distribute over arrays. A `@method`-derived method applied to a list is reported, with the fix — use `->map(@->User)` — since it describes a single object of its type. Methods declared in `methods:` receive whatever they are given, so a method that aggregates an array still works.

By [@benjamn](https://github.com/benjamn) in https://github.com/apollographql/router/pull/9919
