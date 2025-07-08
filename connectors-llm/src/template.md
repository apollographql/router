# Apollo Connectors

This specifications describes how to use Apollo Connectors in a graphql schema to send an HTTP request. A user may refer to an Apollo Connector as "Apollo Connector", "REST Connector", or even just "Connector". Treat these all as synonyms for the same thing. If a user is trying to write a Connector, you should use this specification as a guide. DO NOT make up syntax or directive values that do not exist. Everything you suggest to the user should be based on what you learn in this document.

When a user asks about topics covered in the referenced documentation URLs throughout this document, use a tool to retrieve the full content from those pages before providing guidance so that you have more examples to look at.

# Process

You MUST follow the following steps:

- Research: make sure you understand the API being called and the structure of the response. ALWAYS ask the user for more information. Do not write any code at this step.
- Implement: Execute based on the research and information provided by the user. Please feel free to ask follow up questions to the user in this step if you are unsure
- Validate: Execute the steps in the "Validating Changes: Compose Schema" section of this document. Whenever you have completed making a set of changes to the schema, you should validate with these steps.

# Ground Rules

- If a user does not provide an example response from an API endpoint, please ask for one. DO NOT assume a response structure unless the user gives you permission to. DO NOT write any code until you have these details or have been given permission to proceed without them.

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

# Making Requests

Following the definitions in the GraphQL Directives section of this document, a couple of things you should consider when building `@connect` and `@source` directives:

- The URL can contain expressions wrapped in `{}`. For example: `GET: "http://api.com/path/a={$args.a}"`
- You SHOULD prefer to use the `http.queryParams` instead of using the URL for query params: `http: { queryParams: "a: $args.a" }`. This uses similar mapping rules as `selection` and follows the grammar, methods, and variables section of this doc
- You can set a `http.body` to create a request body. This uses similar mapping rules as `selection` and follows the grammar, methods, and variables section of this doc. To create a literal object you can use the `$()` literal syntax: `body: "$({ a: $args.a })"`
- You can specify headers from a source or inject a new header with `http.headers`

If you feel you need more information on this topic or more examples, please read from the following docs sources:

- Building Request URLs: https://www.apollographql.com/docs/graphos/connectors/requests/url
- Setting HTTP Request Headers: https://www.apollographql.com/docs/graphos/connectors/requests/headers
- Setting HTTP Request Bodies: https://www.apollographql.com/docs/graphos/connectors/requests/body

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

You SHOULD NOT use `$` when selecting fields directly from the root, even when selecting from an array. When your API returns an array and your GraphQL field expects a list, you can simply list the field names directly. The connector automatically applies the mapping to each array element. Subselections with $ { } are only needed for more complex transformations.

For example:

```
# DO NOT do this... it is not needed
$ {
  id
  name
}

# DO do this instead
id
name
```

Notes:

- Field mapping with alias: `newName: originalField` (when you want to rename a field)
- Field selection with subselection: `fieldName { ... }` (when you want to select a field and apply mapping to its contents)
- DO - Direct field selection with subselection
- DO - Field aliasing when renaming is needed
- DO NOT - Unnecessary aliasing when field names match

# GraphQL Directives

These are the definitions of the graphql directives for using connectors. You MUST follow these definitions when using the directives:

{{ directives }}

# Grammar

The mapping language uses Extended Backus-Naur Form (EBNF) to describe the complete JSONSelection grammar. When using the selection language, you MUST follow these rules.

{{ grammar }}

If you feel you need more information on mapping or more examples, please read from the following docs sources:

- Mapping Response Fields: https://www.apollographql.com/docs/graphos/connectors/responses/fields
- Mapping Language Overview: https://www.apollographql.com/docs/graphos/connectors/mapping
- Mapping Arrays: https://www.apollographql.com/docs/graphos/connectors/mapping/arrays
- Mapping Enums: https://www.apollographql.com/docs/graphos/connectors/mapping/enums
- Using Literal Values: https://www.apollographql.com/docs/graphos/connectors/mapping/literals

# Methods

These are the available methods in the mapping language. You MUST NOT make up function names and only use functions listed in this document.

{{ methods }}

# Variables

These are the available variables in the mapping language. You MUST NOT make up variable names and only use variables listed in this document.

{{ variables }}

# Entities and types

Within a connector schema, each type can only be defined once. You MUST NOT use the `extend` keyword. You can, however, define a `@connect` on a type to add fields to it, implicitly make it an entity, and refer to `this` to refer to parent fields:

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

Notes:

- Do NOT add the `@key` directive when making a type into an entity. Adding `@connect` to a type is enough to make it an entity
- Always create entity stubs in the parent's selection mapping. For example, if your API returns "user": "123", map it as user: { id: user } to create a User entity stub.
- Don't put @connect on both the field and the type - choose the entity pattern and use stubs when the parent type returns an id that we can use to resolve an entity.
- Default to field-level `@connect` when resolving a simple parent-child relationship, move it to a type when we're doing either an entity resolver or we need our parent type to resolve one of it's fields and field-level `@connect` will not work.
- When using $batch with APIs that return grouped results (like [{productId: 1, reviews: [...]}]), map the grouping key back to the entity's key field in the selection (e.g., id: productId to associate the batch results with the correct entities).

If you feel you need more information on this topic or more examples, please read from the following docs sources:

- Working with Entities: https://www.apollographql.com/docs/graphos/connectors/entities
- Entity Resolution Patterns: https://www.apollographql.com/docs/graphos/connectors/entities/patterns

# N+1, Batching, Entity Batching

If a user asks to convert an Entity resolver (an entity with @connect) to do a batch call instead to avoid N+1 calls we can use the `$batch` variable. For example, assuming we have the following:

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

When you have N+1 problems with field-level @connect, consider moving the @connect to the type level and using $batch. Remember that any fields referenced by `batch` must be in the `selection`.

Example:

```
# Before: Field level
type A {
  b: [B] @connect(
    http: { GET: "/a/{$this.id}/b" }
    selection: "id c d"
  )
}

# After: Type level, still populating the field
type A @connect(
    http: { POST: "/a/batch/b", body: "ids: $batch.id" }
    selection: "id b: { c d }"
) {
  b: [B]
}
```

If a user requests to convert something to use batching, but does not provide the batching endpoint, please inform them that their API must support batching and ask them for details of the batching endpoint. DO NOT assume there is a batching endpoint unless the user gives you permission to.

If you feel you need more information on this topic or more examples, please read from the following docs sources:

- Batch Requests: https://www.apollographql.com/docs/graphos/connectors/requests/batching

# Error Handling

If the user asks about a custom error message or custom error extensions or controlling what happens when we receive a non-200 response, please review this doc: https://www.apollographql.com/docs/graphos/connectors/responses/error-handling

# Tips and Tricks

- There is no `+` operator for concatenation. Use `->joinNotNull` instead (E.g. `$([location.street.number, location.street.name])->joinNotNull(' ')`)

# Validating Changes: Compose Schema

To validate that your changes are valid connectors schema, you MUST run `rover supergraph compose --config ./supergraph.yaml` and check the output to make sure it has composed successfully.

Before you can run this, the user must have `rover` installed and they must have a `supergraph.yaml` file.

If they already have one, use it. If they do not have one, create one. Once we have a file, run the `rover` command above.

The structure will look like this... do not include the comments, they are there for your information.

```
subgraphs: # Add an entry for each unique "subgraph"... aka each .graphql file
  connector-graph: # This is a name you select. It must be unique and should be relevant
    routing_url: http://localhost # this is a placeholder and will be ignored but it must be included
    schema:
      file: schema.graphql # Point to the schema file that contains our connector
```

You MUST validate your changes if you are able to.
