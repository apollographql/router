### Deliver `@defer` responses under a type condition in a list of mixed types ([PR #10816](https://github.com/apollographql/router/pull/10816))

A `@defer` on a type condition on a list defining an interface implemented by more than one object returned the wrong deferred data:
* If a deferred field was non-null, the deferred data was dropped without an error. The deferred data is now correctly returned.
* If a deferred field was nullable, the router sent deferred data for every object implementing that interface with the field set to `null` for objects that did not have that field. The deferred fields are now sent only for objects of the matching type.


Here is an example of a schema where both `Item` and `Other` implements `Component` returned in a list by `children`:

```graphql
type Query {
  children: [Component!]!
}

interface Component {
  id: ID!
}

type Item implements Component {
  id: ID!
  title: String!
}

type Other implements Component {
  id: ID!
  label: String!
}
```

And this operation:

```graphql
{
  children {
    id
    ... on Item {
      ... @defer(label: "d") { title }
    }
  }
}
```

Before the change, the stream ended with:

```json
{"hasNext": false}
```

It now delivers `title` for the `Item` in the list:

```json
{
  "hasNext": false,
  "incremental": [
    {"label": "d", "data": {"title": "Item 2"}, "path": ["children", 1]}
  ]
}
```

By [@lrlna](https://github.com/lrlna) in https://github.com/apollographql/router/pull/10816
