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

```graphql
directive @connect(
  """
  Optionally references reusable configuration, corresponding
  to `@source(name:)`
  """
  source: String

  "HTTP configuration"
  http: ConnectHTTP!

  "Used to map an API's JSON response to GraphQL fields"
  selection: JSONSelection!

  """
  Allowed only on fields of `Query`. If set to
  `true` the field acts as an entity resolver
  in Apollo Federation
  """
  entity: Boolean

  "Optional batch configuration"
  batch: BatchSettings

  "Optional error handling configuration"
  errors: ConnectorErrors
) repeatable on FIELD_DEFINITION

"Only one of {GET,POST,PUT,PATCH,DELETE} is allowed"
input ConnectHTTP {
  GET: URLPathTemplate
  POST: URLPathTemplate
  PUT: URLPathTemplate
  PATCH: URLPathTemplate
  DELETE: URLPathTemplate

  """
  Header mappings for propagating headers from the
  original client request to the GraphOS Router, or injecting
  specific values.
  """
  headers: [HTTPHeaderMapping!]

  "Mapping from field arguments to POST|PUT|PATCH request bodies"
  body: JSONSelection
}

directive @source(
  """
  Unique identifier for the API this directive
  represents, for example "productsv1"
  """
  name: String!

  "HTTP configuration"
  http: SourceHTTP!

  "Optional error handling configuration for all Connectors for this source"
  errors: ConnectorErrors
) repeatable on SCHEMA

input SourceHTTP {
  """
  The base scheme, hostname, and path to use,
  like "https://api.example.com/v2"
  """
  baseURL: String!

  """
  Default header mappings used for all related
  Connectors. If a Connector specifies its own
  header mappings, that list is merged with this
  one, with the Connector's mappings taking precedence
  when the `name` value matches.
  """
  headers: [HTTPHeaderMapping!]
}

"""
Defines a header for an HTTP request and where its
value comes from.

Only one of {from, value} is allowed
"""
input HTTPHeaderMapping {
  "The name of the header to send to HTTP APIs"
  name: String!

  """
  The name of the header in the original client
  request to the GraphOS Router
  """
  from: String

  "Optional hard-coded value for non-passthrough headers"
  value: String
}

"""
Settings for batching Connectors that use the `$batch` variable to create
requests for multiple entities at once.
"""
input BatchSettings {
  """
  Use this option to limit the number of items in each request. This results in
  (N / maxSize) + 1 requests to your APIs.
  """
  maxSize: Int
}

input ConnectorErrors {
  """
  Use this to configure the "message" field of the top-level GraphQL error that
  occurs when this Connector fails.
  """
  message: JSONSelection

  """
  Use this to configure the "extensions" object of the top-level GraphQL error
  that occurs when this Connector fails.
  """
  extensions: JSONSelection
}

"""
A URL path with optional parameters, mapping to GraphQL
fields or arguments
"""
scalar URLPathTemplate

"A custom syntax for mapping JSON data to GraphQL schema"
scalar JSONSelection
```

# Grammar

The mapping language uses Extended Backus-Naur Form (EBNF) to describe the complete JSONSelection grammar. When using the selection language, you MUST follow these rules.

```
JSONSelection        ::= PathSelection | NamedSelection*
SubSelection         ::= "{" NamedSelection* "}"
NamedSelection       ::= NamedPathSelection | PathWithSubSelection | NamedFieldSelection | NamedGroupSelection
NamedPathSelection   ::= Alias PathSelection
NamedFieldSelection  ::= Alias? Key SubSelection?
NamedGroupSelection  ::= Alias SubSelection
Alias                ::= Key ":"
Path                 ::= VarPath | KeyPath | AtPath | ExprPath
PathSelection        ::= Path SubSelection?
PathWithSubSelection ::= Path SubSelection
VarPath              ::= "$" (NO_SPACE Identifier)? PathStep*
KeyPath              ::= Key PathStep+
AtPath               ::= "@" PathStep*
ExprPath             ::= "$(" LitExpr ")" PathStep*
PathStep             ::= "." Key | "->" Identifier MethodArgs?
Key                  ::= Identifier | LitString
Identifier           ::= [a-zA-Z_] NO_SPACE [0-9a-zA-Z_]*
MethodArgs           ::= "(" (LitExpr ("," LitExpr)* ","?)? ")"
LitExpr              ::= LitPath | LitPrimitive | LitObject | LitArray | PathSelection
LitPath              ::= (LitPrimitive | LitObject | LitArray) PathStep+
LitPrimitive         ::= LitString | LitNumber | "true" | "false" | "null"
LitString            ::= "'" ("\\'" | [^'])* "'" | '"' ('\\"' | [^"])* '"'
LitNumber            ::= "-"? ([0-9]+ ("." [0-9]*)? | "." [0-9]+)
LitObject            ::= "{" (LitProperty ("," LitProperty)* ","?)? "}"
LitProperty          ::= Key ":" LitExpr
LitArray             ::= "[" (LitExpr ("," LitExpr)* ","?)? "]"
NO_SPACE             ::= !SpacesOrComments
SpacesOrComments     ::= (Spaces | Comment)+
Spaces               ::= ("⎵" | "\t" | "\r" | "\n")+
Comment              ::= "#" [^\n]*
```

# Methods

These are the available methods in the mapping language. You MUST NOT make up function names and only use functions listed in this document.

## String methods

| Method | Description | Example |
|---------|---------|---------|
| #### `slice` | Returns a slice of a string | `firstTwoChars: countryCode->slice(0, 2)` |
| #### `size` | Returns the length of a string | `wordLength: word->size` |


## Object methods

| Method | Description | Example |
|---------|---------|---------|
| #### `entries` | Returns a list of key-value pairs | `keyValuePairs: object->entries` |
| #### `size` | Returns the number of properties in an object | `propCount: object->size` |


## Array methods

| Method | Description | Example |
|---------|---------|---------|
| #### `first` | Returns the first value in a list | `firstColor: colors->first` |
| #### `joinNotNull` <MinVersionBadge version="Router v2.3, Federation v2.11" href="https://www.apollographql.com/docs/graphos/connectors/reference/changelog#router-230--composition-2110" /> | Concatenates an array of string values using the specified separator and ignoring `null` values | `$(["a", "b", null, "c"])->joinNotNull(',')` |
| #### `last` | Returns the last value in a list | `lastColor: colors->last` |
| #### `map` | Maps a list of values to a new list, or converts a single item to a list | `colors: colors->map({ name: @ })` |
| #### `slice` | Returns a slice of a list | `firstTwoColors: colors->slice(0, 2)` |
| #### `size` | Returns the length of a list | `colorCount: colors->size` |


## Other methods

| Method | Description | Example |
|---------|---------|---------|
| #### `echo` | Evaluates and returns its first argument | `wrappedValue: value->echo({ wrapped: @ })` |
| #### `jsonStringify` | Converts a value to a JSON string | `jsonBody: body->jsonStringify` |
| #### `match` | Replaces a value with a new value if it matches another value | `status: status->match([1, "one"], [2, "two"], [@, "other"])` |

# Variables

These are the available variables in the mapping language. You MUST NOT make up variable names and only use variables listed in this document.

## Available variables

| Variable | Definition | Availability Notes |
|---------|---------|---------|
| #### `$` | At the top level, `$` refers to the root of the API response body. <br/> Within a `{...}` sub-selection, `$` refers to the value of the parent. [See an example.](#-2) | - Only available in `@connect`'s [`selection`](/graphos/connectors/responses/fields) and [`errors`](/graphos/connectors/responses/error-handling) fields. - Not available in `@source`. |
| #### `$args` | The arguments passed to the field in the GraphQL query. For a field defined like `product(id: ID!): Product`, `$args.id` refers to the `id` argument passed to the `product` field. | - Available in any expression in a `@connect` directive if arguments are defined for the field. - Not available in `@source`. |
| #### `$batch` | Provides a list of entity references so that multiple requests can be batched into a single request. [Learn about batching](/graphos/connectors/requests/batching). | - Only available in `@connect` on types, not on fields. - Not available in `@source`. - Follow these [`$batch` rules to ensure data integrity](/graphos/connectors/requests/batching#rules-for-batch-connectors). |
| #### `$config` | Variables set [in the GraphOS Router configuration](/graphos/connectors/deployment/configuration#accessing-router-configuration-in-connectors). | Always available. |
| #### `$context` | Context set by router customizations like [coprocessors](/graphos/routing/customization/coprocessor). | Only available if router customizations exist where context has been set. |
| #### `$request.headers` <MinVersionBadge version="Router v2.3, Federation v2.11" href="https://www.apollographql.com/docs/graphos/connectors/reference/changelog#router-230--composition-2110" /> | Headers of the incoming request to the router. <br/> Because an HTTP header can contain multiple values, `$request.headers.x` is always an array. Use `->first` to grab the first item: ```mapping showLineNumbers=false $request.headers.authorization->first $request.headers.'x-my-header'->first ``` | Always available. |
| #### `$response.headers` <MinVersionBadge version="Router v2.3, Federation v2.11" href="https://www.apollographql.com/docs/graphos/connectors/reference/changelog#router-230--composition-2110" /> | Headers of the response from the connected HTTP endpoint. <br/> Because an HTTP header can contain multiple values, `$request.headers.x` is always an array. Use `->first` to grab the first item: ```mapping showLineNumbers=false $request.headers.authorization->first $request.headers.'x-my-header'->first ``` | Available in [`selection`](/graphos/connectors/responses/fields) and [`errors`](/graphos/connectors/responses/error-handling). |
| #### `$status` | The numeric HTTP status code (`200`, `404`, etc.) from the response of the connected HTTP endpoint. | - Only available in `@connect`'s [`selection`](/graphos/connectors/responses/fields) and [`errors`](/graphos/connectors/responses/error-handling) fields. - Not available in `@source`. |
| #### `$this` | The parent object of the current field. Can be used to access sibling fields. [Learn about dependencies `$this` can create.](#this-1) | - Only available on non-root types, that is, not within `Query` or `Mutation` Connectors. - Not available in `@source`. |
| #### `@` | The value being transformed with a [method](#methods). Behaves differently depending on the context. [Learn more.](#-3) | Depends on the specific transformation method or mapping being applied. |

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
