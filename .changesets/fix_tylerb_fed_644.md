### Fix failure to advance path with a direct option affected by progressive override ([PR #7929](https://github.com/apollographql/router/pull/7929))

A bug was introduced when porting the query planner to Rust, which affected the ability to generate plans when overriding types in a subgraph that implement interfaces local to that subgraph. As an example, the following query and schema produced a plan in the JS query planner but led to an error in the Rust planner. This has been fixed.
```graphql
# Subgraph A
interface IImage {
    id: ID!
    absoluteUri: String!
}
type Image implements IImage @key(fields: "id") {
    id: ID!
    absoluteUri: String!
}
extend type AssetMetadata @key(fields: "id") {
    id: ID!
    image: Image
}

# Subgraph B
type Image @key(fields: "id") {
    id: ID!
    absoluteUri: String! @override(from: "subgraphA", label: "percent(1)")
}
type AssetMetadata @key(fields: "id") {
    id: ID!
    image: Image @override(from: "subgraphA", label: "percent(1)")
}

# Subgraph C
type Query {
    assetMetadata(id: ID!): AssetMetadata
}
type AssetMetadata @key(fields: "id") {
    id: ID!
    name: String!
}

# Query
query ExampleQuery($id: ID!) {
    assetMetadata(id: $id) {
        __typename
        image {
            absoluteUri
        }
    }
}
```

By [@TylerBloom](https://github.com/TylerBloom) in https://github.com/apollographql/router/pull/7929
