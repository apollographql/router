# Apollo Connectors

This specifications describes how to use Apollo Connectors in a graphql schema to send an HTTP request. A user may refer to an Apollo Connector as "Apollo Connector", "REST Connector", or even just "Connector". Treat these all as synonyms for the same thing. If a user is trying to write a Connector, you should use this specification as a guide. DO NOT make up syntax or directive values that do not exist. Everything you suggest to the user should be based on what you learn in this document.

# Basics

The purpose of a connector is to declaratively mark up a graphql schema with directives which result in HTTP requests being sent to an API. The response of this API is then mapped by the connector to the graphql schema.

To use connectors, a `.graphql` file must be used and must include the following at the top of the file which sets federation to `2.11` and imports the `@source` and `@connect` directives (outlined under the GraphQL Directives section of this document):

```
extend schema
  @link(url: "https://specs.apollo.dev/federation/v2.11")
  @link(url: "https://specs.apollo.dev/connect/v0.2", import: ["@source", "@connect"])
```

The `@source` directive is used to define an API we will be calling. You SHOULD create a single `@source` per unique base URL and reference the `name` as the `source` for a `@connect`.

For example:

```graphql
@source(name: "random_person_api", http: { baseURL: "https://randomuser.me/api/" })

type User {
  firstName: String
  lastName: String
}

type Query {
  usersLocalSource(number: Int): [User]
    @connect(
      source: "random_person_api"
      http: {
        GET: "/api"
      }
      selection: """
      $.results {
        firstName: name.first
        lastName: name.last
      }
      """
    )
}
```

In this example, the `selection` is the mapping from the REST HTTP response (JSON) to the graphql schema. You MUST follow the mapping language as outlined in the "Grammar" section of this document and can use the "Methods" and "Variables" outlined in this document.

# Sub Selections

When mapping, you SHOULD prefer to use a "subselection" instead of a a `->map` function. A "subselection" will already create an object so you do not need to worry about creating an object literal. It will also create a list of objects if you're running a subselection against an array of items.

```
# DO... for a single item OR an array
$.results {
    firstName: name.first
    lastName: name.last
}

# DO NOT (unless absolutely required)
$.results.map({
    firstName: name.first,
    lastName: name.last
})
```

# GraphQL Directives

These are the definitions of the graphql directives for using connectors. You MUST follow these definitions when using the directives:

{{ directives }}

# Grammar

The mapping language uses Extended Backus-Naur Form (EBNF) to describe the complete JSONSelection grammar. When using the selection language, you MUST follow these rules.

{{ grammar }}

# Methods

These are the available methods in the mapping language. You MUST NOT make up function names and only use functions listed in this document.

{{ methods }}

# Variables

These are the available variables in the mapping language. You MUST NOT make up variable names and only use variables listed in this document.

{{ variables }}

# Entities and types

Within a connector schema, each type can only be defined once. You MUST NOT use the `extend` keyword. You can, however, define a `@connect` on a type to add fields to it and refer to `this` to refer to parent fields:

```
type MyType @connect(http: { GET: "/api/{$this.id}"}, selection: "myOtherField") {
    id: ID
    myOtherField: String
}
```

You can define an entity "stub" somewhere else in your schema that will then trigger this connector:

```
type myOtherType {
    a: String
    b: MyType
}

type Query {
    myQuery: MyType @connect(selection: """
        a
        b: {
            id: bId
        }
    """)
}
```

When using entity types with `@connect`, create entity stubs in the parent type's selection by mapping just the key fields needed for the entity to resolve itself (e.g., testing: { id: id.value }).

# Entity Batching

If a user asks to convert an Entity resolver (@connect) to do a batch call instead to avoid N+1 calls we can use the `$batch` variable. For example, assuming we have the following:

```
type Testing @connect(
  source: "localhost_api"
  http: {
    GET: "/api/user/{$this.id}"
  }
  selection: """
  id
  testField
  """
) {
  id: Int
  testField: String
}
```

If the user gives us something ike `/batch` as the URL and tells us we can put the ids in the body, we can do this:

```
type Testing @connect(
  source: "localhost_api"
  http: {
    GET: "/batch"
    body: "ids: $batch.id"
  }
  selection: """
  id
  testField
  """
) {
  id: Int
  testField: String
}
```

Notice we did NOT change the selection, only the `http`.

# Tips and Tricks

- There is no `+` operator for concatenation. Use `->joinNotNull` instead (E.g. `$([location.street.number, location.street.name])->joinNotNull(' ')`)
