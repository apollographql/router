# What is `JSONSelection` syntax?

One of the most fundamental goals of the connectors project is that a GraphQL
subgraph schema, all by itself, should be able to encapsulate and selectively
re-expose any JSON-speaking data source as strongly-typed GraphQL, using a
declarative annotation syntax based on the `@source` and `@connect` directives,
with no need for additional resolver code, and without having to run a subgraph
server.

Delivering on this goal entails somehow transforming arbitrary JSON into
GraphQL-shaped JSON without writing any procedural transformation code. Instead,
these transformations are expressed using a static, declarative string literal
syntax, which resembles GraphQL operation syntax but also supports a number of
other features necessary/convenient for processing arbitrary JSON.

The _static_ part is important, since we need to be able to tell, by examining a
given `JSONSelection` string at composition time, exactly what shape its output
will have, even though we cannot anticipate every detail of every possible JSON
input that will be encountered at runtime. As a benefit of this static analysis,
we can then validate that the connector schema reliably generates the expected
GraphQL data types.

In GraphQL terms, this syntax is represented by the `JSONSelection` scalar type,
whose grammar and semantics are detailed in this document. Typically, string
literals obeying this grammar will be passed as the `selection` argument to the
`@connect` directive, which is used to annotate fields of object types within a
subgraph schema.

In terms of this Rust implementation, the string syntax is parsed into a
`JSONSelection` enum, which implements the `ApplyTo` trait for processing
incoming JSON and producing GraphQL-friendly JSON output.

## Guiding principles

As the `JSONSelection` syntax was being designed, and as we consider future
improvements, we should adhere to the following principles:

1. Since `JSONSelection` syntax resembles GraphQL operation syntax and will
   often be used in close proximity to GraphQL operations, whenever an element
   of `JSONSelection` syntax looks the same as GraphQL, its behavior and
   semantics should be the same as (or at least analogous to) GraphQL. It is
   preferable, therefore, to invent new (non-GraphQL) `JSONSelection` syntax
   when we want to introduce behaviors that are not part of GraphQL, or when
   GraphQL syntax is insufficiently expressive to accomplish a particular
   JSON-processing task. For example, `->` method syntax is better for inline
   transformations that reusing/abusing GraphQL field argument syntax.

2. It must be possible to statically determine the output shape (object
   properties, array types, and nested value shapes) produced by a
   `JSONSelection` string. JSON data encountered at runtime may be inherently
   dynamic and unpredicatable, but we must be able to validate the output shape
   matches the GraphQL schema. Because we can assume all input data is some kind
   of JSON, for types whose shape cannot be statically determined, the GraphQL
   `JSON` scalar type can be used as an "any" type, though this should be
   avoided because it limits the developer's ability to subselect fields of the
   opaque `JSON` value in GraphQL operations.

3. Backwards compatibility should be maintained as we release new versions of
   the `JSONSelection` syntax along with new versions of the (forthcoming)
   `@link(url: "https://specs.apollo.dev/connect/vX.Y")` specification. Wherever
   possible, we should only add new functionality, not remove or change existing
   functionality, unless we are releasing a new major version (and even then we
   should be careful not to create unnecessary upgrade work for developers).

## Formal grammar

[Extended Backus-Naur Form](https://en.wikipedia.org/wiki/Extended_Backus%E2%80%93Naur_form)
(EBNF) provides a compact way to describe the complete `JSONSelection` grammar.

This grammar is more for future reference than initial explanation, so don't
worry if it doesn't seem helpful yet, as every rule will be explained in detail
below.

```ebnf
JSONSelection        ::= NakedSubSelection | PathSelection
NakedSubSelection    ::= NamedSelection* StarSelection?
SubSelection         ::= "{" NakedSubSelection "}"
NamedSelection       ::= NamedPathSelection | NamedFieldSelection | NamedQuotedSelection | NamedGroupSelection
NamedPathSelection   ::= Alias PathSelection
NamedFieldSelection  ::= Alias? Identifier SubSelection?
NamedQuotedSelection ::= Alias StringLiteral SubSelection?
NamedGroupSelection  ::= Alias SubSelection
Alias                ::= Identifier ":"
PathSelection        ::= (VarPath | KeyPath) SubSelection?
VarPath              ::= "$" (NO_SPACE Identifier)? PathStep*
KeyPath              ::= Key PathStep+
PathStep             ::= "." Key | "->" Identifier MethodArgs?
Key                  ::= Identifier | StringLiteral
Identifier           ::= [a-zA-Z_] NO_SPACE [0-9a-zA-Z_]*
StringLiteral        ::= "'" ("\\'" | [^'])* "'" | '"' ('\\"' | [^"])* '"'
MethodArgs           ::= "(" (JSLiteral ("," JSLiteral)*)? ")"
JSLiteral            ::= JSPrimitive | JSObject | JSArray | PathSelection
JSPrimitive          ::= StringLiteral | JSNumber | "true" | "false" | "null"
JSNumber             ::= "-"? (UnsignedInt ("." [0-9]*)? | "." [0-9]+)
UnsignedInt          ::= "0" | [1-9] NO_SPACE [0-9]*
JSObject             ::= "{" (JSProperty ("," JSProperty)*)? "}"
JSProperty           ::= Key ":" JSLiteral
JSArray              ::= "[" (JSLiteral ("," JSLiteral)*)? "]"
StarSelection        ::= Alias? "*" SubSelection?
NO_SPACE             ::= !SpacesOrComments
SpacesOrComments     ::= (Spaces | Comment)+
Spaces               ::= (" " | "\t" | "\r" | "\n")+
Comment              ::= "#" [^\n]*
```

### How to read this grammar

Every valid `JSONSelection` string can be parsed by starting with the
`JSONSelection` non-terminal and repeatedly applying one of the expansions on
the right side of the `::=` operator, with alternatives separated by the `|`
operator. Every `CamelCase` identifier on the left side of the `::=` operator
can be recursively expanded into one of its right-side alternatives.

Methodically trying out all these alternatives is the fundamental job of the
parser. Parsing succeeds when only terminal tokens remain (quoted text or
regular expression character classes).

Ambiguities can be resolved by applying the alternatives left to right,
accepting the first set of expansions that fully matches the input tokens. An
example where this kind of ordering matters is the `NamedSelection` rule, which
specifies parsing `NamedPathSelection` before `NamedFieldSelection` and
`NamedQuotedSelection`, so the entire path will be consumed, rather than
mistakenly consuming only the first key in the path as a field name.

As in many regular expression syntaxes, the `*` and `+` operators denote
repetition (_zero or more_ and _one or more_, respectively), `?` denotes
optionality (_zero or one_), parentheses allow grouping, `"quoted"` or
`'quoted'` text represents raw characters that cannot be expanded further, and
`[...]` specifies character ranges.

### Whitespace, comments, and `NO_SPACE`

In many parsers, whitespace and comments are handled by the lexer, which
performs tokenization before the parser sees the input. This approach can
simplify the grammar, because the parser doesn't need to worry about whitespace
or comments, and can focus instead on parsing the structure of the input tokens.

The grammar shown above adopts this convention. In other words, instead of
explicitly specifying everywhere whitespace and comments are allowed, we
verbally declare that **whitespace and comments are _allowed_ between any
tokens, except where explicitly forbidden by the `NO_SPACE` notation**. The
`NO_SPACE ::= !SpacesOrComments` rule is called _negative lookahead_ in many
parsing systems. Spaces are also implicitly _required_ if omitting them would
undesirably result in parsing adjacent tokens as one token, though the grammar
cannot enforce this requirement.

While the current Rust parser implementation does not have a formal lexical
analysis phase, the `spaces_or_comments` function is used extensively to consume
whitespace and `#`-style comments wherever they might appear between tokens. The
negative lookahead of `NO_SPACE` is enforced by _avoiding_ `spaces_or_comments`
in a few key places:

```ebnf
VarPath     ::= "$" (NO_SPACE Identifier)? PathStep*
Identifier  ::= [a-zA-Z_] NO_SPACE [0-9a-zA-Z_]*
UnsignedInt ::= "0" | [1-9] NO_SPACE [0-9]*
```

These rules mean the `$` of a `$variable` cannot be separated from the
identifier part (so `$ var` is invalid), and the first character of a
multi-character `Identifier` or `UnsignedInt` must not be separated from the
remaining characters.

Make sure you use `spaces_or_comments` generously when modifying or adding to
the grammar implementation, or parsing may fail in cryptic ways when the input
contains seemingly harmless whitespace or comment characters.

### GraphQL string literals vs. `JSONSelection` string literals

Since the `JSONSelection` syntax is meant to be embedded within GraphQL string
literals, and GraphQL shares the same `'...'` and `"..."` string literal syntax
as `JSONSelection`, it can be visually confusing to embed a `JSONSelection`
string literal (denoted by the `StringLiteral` non-terminal) within a GraphQL
string.

Fortunately, GraphQL also supports multi-line string literals, delimited by
triple quotes (`"""` or `'''`), which allow using single- or double-quoted
`JSONSelection` string literals freely within the GraphQL string, along with
newlines and `#`-style comments.

While it can be convenient to write short `JSONSelection` strings inline using
`"` or `'` quotes at the GraphQL level, multi-line string literals are strongly
recommended (with comments!) for any `JSONSelection` string that would overflow
the margin of a typical text editor.

## Rule-by-rule grammar explanation

This section discusses each non-terminal production in the `JSONSelection`
grammar, using a visual representation of the EBNF syntax called "railroad
diagrams" to illustrate the possible expansions of each rule. In case you need
to generate new diagrams or regenerate existing ones, you can use [this online
generator](https://rr.red-dove.com/ui), whose source code is available
[here](https://github.com/GuntherRademacher/rr).

The railroad metaphor comes from the way you read the diagram: start at the ▶▶
arrows on the far left side, and proceed along any path a train could take
(without reversing) until you reach the ▶◀ arrows on the far right side.
Whenever your "train" stops at a non-terminal node, recursively repeat the
process using the diagram for that non-terminal. When you reach a terminal
token, the input must match that token at the current position to proceed. If
you get stuck, restart from the last untaken branch. The input is considered
valid if you can reach the end of the original diagram, and invalid if you
exhaust all possible alternatives without reaching the end.

I like to think every stop along the railroad has a gift shop and restrooms, so
feel free to take your time and enjoy the journey.

### `JSONSelection ::=`

![JSONSelection](./grammar/JSONSelection.svg)

The `JSONSelection` non-terminal is the top-level entry point for the grammar,
and appears nowhere else within the rest of the grammar. It can be either a
`NakedSubSelection` (for selecting multiple named items) or a `PathSelection`
(for selecting a single anonymous value from a given path). When the
`PathSelection` option is chosen at this level, the entire `JSONSelection` must
be that single path, without any other named selections.

### `NakedSubSelection ::=`

![NakedSubSelection](./grammar/NakedSubSelection.svg)

A `NakedSubSelection` is a `SubSelection` without the surrounding `{` and `}`
braces. It can appear at the top level of a `JSONSelection`, but otherwise
appears only as part of the `SubSelection` rule, meaning it must have braces
everywhere except at the top level.

Because a `NakedSubSelection` can contain any number of `NamedSelection` items
(including zero), and may have no `StarSelection`, it's possible for the
`NakedSelection` to be fully empty. In these unusual cases, whitespace and
comments are still allowed, and the result of the selection will always be an
empty object.

In the Rust implementation, there is no dedicated `NakedSubSelection` struct, as
we use the `SubSelection` struct to represent the meaningful contents of the
selection, regardless of whether it has braces. The `NakedSubSelection`
non-terminal is just a grammatical convenience, to avoid repetition between
`JSONSelection` and `SubSelection`.

### `SubSelection ::=`

![SubSelection](./grammar/SubSelection.svg)

A `SubSelection` is a `NakedSubSelection` surrounded by `{` and `}`, and is used
to select specific properties from the preceding object, much like a nested
selection set in a GraphQL operation.

Note that `SubSelection` may appear recursively within itself, as part of one of
the various `NamedSelection` rules. This recursion allows for arbitrarily deep
nesting of selections, which is necessary to handle complex JSON structures.

### `NamedSelection ::=`

![NamedSelection](./grammar/NamedSelection.svg)

Every possible production of the `NamedSelection` non-terminal corresponds to a
named property in the output object, though each one obtains its value from the
input object in a slightly different way.

### `NamedPathSelection ::=`

![NamedPathSelection](./grammar/NamedPathSelection.svg)

Since `PathSelection` returns an anonymous value extracted from the given path,
if you want to use a `PathSelection` alongside other `NamedSelection` items, you
have to prefix it with an `Alias`, turning it into a `NamedPathSelection`.

For example, you cannot omit the `pathName:` alias in the following
`NakedSubSelection`, because `some.nested.path` has no output name by itself:

```graphql
position { x y }
pathName: some.nested.path { a b c }
scalarField
```

The ordering of alternatives in the `NamedSelection` rule is important, so the
`NamedPathSelection` alternative can be considered before `NamedFieldSelection`
and `NamedQuotedSelection`, because a `NamedPathSelection` such as `pathName:
some.nested.path` has a prefix that looks like a `NamedFieldSelection`:
`pathName: some`, causing an error when the parser encounters the remaining
`.nested.path` text. Some parsers would resolve this ambiguity by forbidding `.`
in the lookahead for `Named{Field,Quoted}Selection`, but negative lookahead is
tricky for this parser (see similar discussion regarding `NO_SPACE`), so instead
we greedily parse `NamedPathSelection` first, when possible, since that ensures
the whole path will be consumed.

### `NamedFieldSelection ::=`

![NamedFieldSelection](./grammar/NamedFieldSelection.svg)

The `NamedFieldSelection` non-terminal is the option most closely resembling
GraphQL field selections, where the field name must be an `Identifier`, may have
an `Alias`, and may have a `SubSelection` to select nested properties (which
requires the field's value to be an object rather than a scalar).

In practice, whitespace is often required to keep multiple consecutive
`NamedFieldSelection` identifiers separate, but is not strictly necessary when
there is no ambiguity, as when an identifier follows a preceding subselection:
`a{b}c`.

### `NamedQuotedSelection ::=`

![NamedQuotedSelection](./grammar/NamedQuotedSelection.svg)

Since arbitrary JSON objects can have properties that are not identifiers, we
need a version of `NamedFieldSelection` that allows for quoted property names as
opposed to identifiers.

However, since our goal is always to produce an output that is safe for GraphQL
consumption, an `Alias` is strictly required in this case, and it must be a
valid GraphQL `Identifier`:

```graphql
first
second: "second property" { x y z }
third { a b }
```

Besides extracting the `first` and `third` fields in typical GraphQL fashion,
this selection extracts the `second property` field as `second`, subselecting
`x`, `y`, and `z` from the extracted object. The final object will have the
properties `first`, `second`, and `third`.

### `NamedGroupSelection ::=`

![NamedGroupSelection](./grammar/NamedGroupSelection.svg)

Sometimes you will need to take a group of named properties and nest them under
a new name in the output object. The `NamedGroupSelection` syntax allows you to
provide an `Alias` followed by a `SubSelection` that contains the named
properties to be grouped. The `Alias` is mandatory because the grouped object
would otherwise be anonymous.

For example, if the input JSON has `firstName` and `lastName` fields, but you
want to represent them under a single `names` field in the output object, you
could use the following `NamedGroupSelection`:

```graphql
names: {
  first: firstName
  last: lastName
}
# Also allowed:
firstName
lastName
```

A common use case for `NamedGroupSelection` is to create nested objects from
scalar ID fields:

```graphql
postID
title
author: {
  id: authorID
  name: authorName
}
```

This convention is useful when the `Author` type is an entity with `@key(fields:
"id")`, and you want to select fields from `post` and `post.author` in the same
query, without directly handling the `post.authorID` field in GraphQL.

### `Alias ::=`

![Alias](./grammar/Alias.svg)

Analogous to a GraphQL alias, the `Alias` syntax allows for renaming properties
from the input JSON to match the desired output shape.

In addition to renaming, `Alias` can provide names to otherwise anonymous
structures, such as those selected by `PathSelection`, `NamedGroupSelection`, or
`StarSelection` syntax.

Because we always want to generate GraphQL-safe output properties, an `Alias`
must be a valid GraphQL identifier, rather than a quoted string.

### `PathSelection ::=`

![PathSelection](./grammar/PathSelection.svg)

A `PathSelection` is a `VarPath` or `KeyPath` followed by an optional
`SubSelection`. The purpose of a `PathSelection` is to extract a single
anonymous value from the input JSON, without preserving the nested structure of
the keys along the path.

Since properties along the path may be either `Identifier` or `StringLiteral`
values, you are not limited to selecting only properties that are valid GraphQL
field names, e.g. `myID: people."Ben Newman".id`. This is a slight departure
from JavaScript syntax, which would use `people["Ben Newman"].id` to achieve the
same result. Using `.` for all steps along the path is more consistent, and
aligns with the goal of keeping all property names statically analyzable, since
it does not suggest dynamic properties like `people[$name].id` are allowed.

Often, the whole `JSONSelection` string serves as a `PathSelection`, in cases
where you want to extract a single nested value from the input JSON, without
selecting any other named properties:

```graphql
type Query {
  authorName(isbn: ID!): String @connect(
    source: "BOOKS"
    http: { GET: "/books/{$args.isbn}"}
    selection: "author.name"
  )
}
```

If you need to select other named properties, you can still use a
`PathSelection` as part of a `NakedSubSelection`, as long as you give it an
`Alias`:

```graphql
type Query {
  book(isbn: ID!): Book @connect(
    source: "BOOKS"
    http: { GET: "/books/{$args.isbn}"}
    selection: """
      title
      year: publication.year
      authorName: author.name
    """
  )
}
```

### `VarPath ::=`

![VarPath](./grammar/VarPath.svg)

A `VarPath` is a `PathSelection` that begins with a `$variable` reference, which
allows embedding arbitrary variables and their sub-properties within the output
object, rather than always selecting a property from the input object. The
`variable` part must be an `Identifier`, and must not be separated from the `$`
by whitespace.

In the Rust implementation, input variables are passed as JSON to the
`apply_with_vars` method of the `ApplyTo` trait, providing additional context
besides the input JSON. Unlike GraphQL, the provided variables do not all have
to be consumed, since variables like `$this` may have many more possible keys
than you actually want to use.

Variable references are especially useful when you want to refer to field
arguments (like `$args.some.arg` or `$args { x y }`) or sibling fields of the
current GraphQL object (like `$this.sibling` or `sibs: $this { brother sister
}`).

Injecting a known argument value comes in handy when your REST endpoint does not
return the property you need:

```graphql
type Query {
  user(id: ID!): User @connect(
    source: "USERS"
    http: { GET: "/users/{$args.id}"}
    selection: """
      # For some reason /users/{$args.id} returns an object with name
      # and email but no id, so we inject the id manually:
      id: $args.id
      name
      email
    """
  )
}

type User @key(fields: "id") {
  id: ID!
  name: String
  email: String
}
```

In addition to variables like `$this` and `$args`, a special `$` variable is
always bound to the current value being processed, which allows you to transform
input data that looks like this

```json
{
  "id": 123,
  "name": "Ben",
  "friend_ids": [234, 345, 456]
}
```

into output data that looks like this

```json
{
  "id": 123,
  "name": "Ben",
  "friends": [
    { "id": 234 },
    { "id": 345 },
    { "id": 456 }
  ]
}
```

using the following `JSONSelection` string:

```graphql
id name friends: friend_ids { id: $ }
```

Because `friend_ids` is an array, the `{ id: $ }` selection maps over each
element of the array, with `$` taking on the value of each scalar ID in turn.
See [the FAQ](#what-about-arrays) for more discussion of this array-handling
behavior.

The `$` variable is also essential for disambiguating a `KeyPath` consisting of
only one key from a `NamedFieldSelection` with no `Alias`. For example,
`$.result` extracts the `result` property as an anonymous value from the current
object, where as `result` would select an object that still has the `result`
property.

### `KeyPath ::=`

![KeyPath](./grammar/KeyPath.svg)

A `KeyPath` is a `PathSelection` that begins with a `Key` (referring to a
property of the current object) and is followed by a sequence of at least one
`PathStep`, where each `PathStep` either selects a nested key or invokes a `->`
method against the preceding value.

For example:

```graphql
items: data.nested.items { id name }
firstItem: data.nested.items->first { id name }
firstItemName: data.nested.items->first.name
```

An important ambiguity arises when you want to extract a `PathSelection`
consisting of only a single key, such as `data` by itself. Since there is no `.`
to disambiguate the path from an ordinary `NamedFieldSelection`, the `KeyPath`
rule is inadequate. Instead, you should use a `VarPath` (which also counts as a
`PathSelection`), where the variable is the special `$` character, which
represents the current value being processed:

```graphql
$.data { id name }
```

This will produce a single object with `id` and `name` fields, without the
enclosing `data` property. Equivalently, you could manually unroll this example
to the following `NakedSubSelection`:

```graphql
id: data.id
name: data.name
```

In this case, the `$.` is no longer necessary because `data.id` and `data.name`
are unambiguously `KeyPath` selections.

> For backwards compatibility with earlier versions of the `JSONSelection`
syntax that did not support the `$` variable, you can also use a leading `.`
character (so `.data { id name }`, or even `.data.id` or `.data.name`) to mean
the same thing as `$.`, but this is no longer recommended, since `.data` is easy
to mistype and misread, compared to `$.data`.

### `PathStep ::=`

![PathStep](./grammar/PathStep.svg)

A `PathStep` is a single step along a `VarPath` or `KeyPath`, which can either
select a nested key using `.` or invoke a method using `->`.

Keys selected using `.` can be either `Identifier` or `StringLiteral` names, but
method names invoked using `->` must be `Identifier` names, and must be
registered in the `JSONSelection` parser in order to be recognized.

For the time being, only a fixed set of known methods are supported, though this
list may grow and/or become user-configurable in the future:

> Full disclosure: even this list is still aspirational, but suggestive of the
> kinds of methods that are likely to be supported in the next version of the
> `JSONSelection` parser.

```graphql
list->first { id name }
list->last.name
list->slice($args.start, $args.end)
list->reverse
some.value->times(2)
some.value->plus($addend)
some.value->minus(100)
some.value->div($divisor)
isDog: kind->eq("dog")
isNotCat: kind->neq("cat")
__typename: kind->match({ "dog": "Dog", "cat": "Cat" })
decoded: utf8Bytes->decode("utf-8")
utf8Bytes: string->encode("utf-8")
encoded: bytes->encode("base64")
```

### `MethodArgs ::=`

![MethodArgs](./grammar/MethodArgs.svg)

When a `PathStep` invokes an `->operator` method, the method invocation may
optionally take a sequence of comma-separated `JSLiteral` arguments in
parentheses, as in `list->slice(0, 5)` or `kilometers: miles->times(1.60934)`.

Methods do not have to take arguments, as in `list->first` or `list->last`,
which is why `MethodArgs` is optional in `PathStep`.

### `Key ::=`

![Key](./grammar/Key.svg)

A property name occurring along a dotted `PathSelection`, either an `Identifier`
or a `StringLiteral`.

### `Identifier ::=`

![Identifier](./grammar/Identifier.svg)

Any valid GraphQL field name. If you need to select a property that is not
allowed by this rule, use a `NamedQuotedSelection` instead.

In some languages, identifiers can include `$` characters, but `JSONSelection`
syntax aims to match GraphQL grammar, which does not allow `$` in field names.
Instead, the `$` is reserved for denoting variables in `VarPath` selections.

### `StringLiteral ::=`

![StringLiteral](./grammar/StringLiteral.svg)

A string literal that can be single-quoted or double-quoted, and may contain any
characters except the quote character that delimits the string. The backslash
character `\` can be used to escape the quote character within the string.

Note that the `\\'` and `\\"` tokens correspond to character sequences
consisting of two characters: a literal backslash `\` followed by a single quote
`'` or double quote `"` character, respectively. The double backslash is
important so the backslash can stand alone, without escaping the quote
character.

You can avoid most of the headaches of escaping by choosing your outer quote
characters wisely. If your string contains many double quotes, use single quotes
to delimit the string, and vice versa, as in JavaScript.

### `JSLiteral ::=`

![JSLiteral](./grammar/JSLiteral.svg)

A `JSLiteral` represents a JSON-like value that can be passed inline as part of
`MethodArgs`.

The `JSLiteral` mini-language diverges from JSON by allowing symbolic
`PathSelection` values (which may refer to variables or fields) in addition to
the usual JSON primitives. This allows `->` methods to be parameterized in
powerful ways, e.g. `page: list->slice(0, $limit)`.

Also, as a minor syntactic convenience, `JSObject` literals can have
`Identifier` or `StringLiteral` keys, whereas JSON objects can have only
double-quoted string literal keys.

### `JSPrimitive ::=`

![JSPrimitive](./grammar/JSPrimitive.svg)

Analogous to a JSON primitive value, with the only differences being that
`JSNumber` does not currently support the exponential syntax, and
`StringLiteral` values can be single-quoted as well as double-quoted.

### `JSNumber ::=`

![JSNumber](./grammar/JSNumber.svg)

A numeric literal that is possibly negative and may contain a fractional
component. The integer component is required unless a fractional component is
present, and the fractional component can have zero digits when the integer
component is present (as in `-123.`), but the fractional component must have at
least one digit when there is no integer component, since `.` is not a valid
numeric literal by itself. Leading and trailing zeroes are essential for the
fractional component, but leading zeroes are disallowed for the integer
component, except when the integer component is exactly zero.

### `UnsignedInt ::=`

![UnsignedInt](./grammar/UnsignedInt.svg)

The integer component of a `JSNumber`, which must be either `0` or an integer
without any leading zeroes.

### `JSObject ::=`

![JSObject](./grammar/JSObject.svg)

A sequence of `JSProperty` items within curly braces, as in JavaScript.

Trailing commas are not currently allowed, but could be supported in the future.

### `JSProperty ::=`

![JSProperty](./grammar/JSProperty.svg)

A key-value pair within a `JSObject`. Note that the `Key` may be either an
`Identifier` or a `StringLiteral`, as in JavaScript. This is a little different
from JSON, which allows double-quoted strings only.

### `JSArray ::=`

![JSArray](./grammar/JSArray.svg)

A list of `JSLiteral` items within square brackets, as in JavaScript.

Trailing commas are not currently allowed, but could be supported in the future.

### `StarSelection ::=`

![StarSelection](./grammar/StarSelection.svg)

The `StarSelection` non-terminal is uncommon when working with GraphQL, since it
selects all remaining properties of an object, which can be difficult to
represent using static GraphQL types, without resorting to the catch-all `JSON`
scalar type. Still, a `StarSelection` can be useful for consuming JSON
dictionaries with dynamic keys, or for capturing unexpected properties for
debugging purposes.

When used, a `StarSelection` must come after any `NamedSelection` items within a
given `NakedSubSelection`.

A common use case for `StarSelection` is capturing all properties not otherwise
selected using a field called `allOtherFields`, which must have a generic `JSON`
type in the GraphQL schema:

```graphql
knownField
anotherKnownField
allOtherFields: *
```

Note that `knownField` and `anotherKnownField` will not be included in the
`allOtherFields` output, since they are selected explicitly. In this sense, the
`*` functions a bit like object `...rest` syntax in JavaScript.

If you happen to know these other fields all have certain properties, you can
restrict the `*` selection to just those properties:

```graphql
knownField { id name }
allOtherFields: * { id }
```

Sometimes a REST API will return a dictionary result with an unknown set of
dynamic keys but values of some known type, such as a map of ISBN numbers to
`Book` objects:

```graphql
booksByISBN: result.books { * { title author { name } }
```

Because the set of ISBN numbers is statically unknowable, the type of
`booksByISBN` would have to be `JSON` in the GraphQL schema, but it can still be
useful to select known properties from the `Book` objects within the
`result.books` dictionary, so you don't return more GraphQL data than necessary.

The grammar technically allows a `StarSelection` with neither an `Alias` nor a
`SubSelection`, but this is not a useful construct from a GraphQL perspective,
since it provides no output fields that can be reliably typed by a GraphQL
schema. This form has some use cases when working with `JSONSelection` outside
of GraphQL, but they are not relevant here.

### `NO_SPACE ::= !SpacesOrComments`

The `NO_SPACE` non-terminal is used to enforce the absence of whitespace or
comments between certain tokens. See [Whitespace, comments, and
`NO_SPACE`](#whitespace-comments-and-no_space) for more information. There is no
diagram for this rule because the `!` negative lookahead operator is not
supported by the railroad diagram generator.

### `SpacesOrComments ::=`

![SpacesOrComments](./grammar/SpacesOrComments.svg)

A run of either whitespace or comments involving at least one character, which
are handled equivalently (ignored) by the parser.

### `Spaces ::=`

![Spaces](./grammar/Spaces.svg)

A run of at least one whitespace character, including spaces, tabs, carriage
returns, and newlines.

Note that we generally allow any amount of whitespace between tokens, so the
`Spaces` non-terminal is not explicitly used in most places where whitespace is
allowed, though it could be used to enforce the presence of some whitespace, if
desired.

### `Comment ::=`

![Comment](./grammar/Comment.svg)

A `#` character followed by any number of characters up to the next newline
character. Comments are allowed anywhere whitespace is allowed, and are handled
like whitespace (i.e. ignored) by the parser.

## FAQ

### What about arrays?

As with standard GraphQL operation syntax, there is no explicit representation
of array-valued fields in this grammar, but (as with GraphQL) a `SubSelection`
following an array-valued field or `PathSelection` will be automatically applied
to every element of the array, thereby preserving/mapping/sub-selecting the
array structure.

Conveniently, this handling of arrays also makes sense within dotted
`PathSelection` elements, which do not exist in GraphQL. Consider the following
selections, assuming the `author` property of the JSON object has an object
value with a child property called `articles` whose value is an array of
`Article` objects, which have `title`, `date`, `byline`, and `author`
properties:

```graphql
@connect(
  selection: "author.articles.title" #1
  selection: "author.articles { title }" #2
  selection: "author.articles { title date }" #3
  selection: "author.articles.byline.place" #4
  selection: "author.articles.byline { place date }" #5
  selection: "author.articles { name: author.name place: byline.place }" #6
  selection: "author.articles { titleDateAlias: { title date } }" #7
)
```

These selections should produce the following result shapes:

1. an array of `title` strings
2. an array of `{ title }` objects
3. an array of `{ title date }` objects
4. an array of `place` strings
5. an array of `{ place date }` objects
6. an array of `{ name place }` objects
7. an array of `{ titleDateAlias }` objects

If the `author.articles` value happened not to be an array, this syntax would
resolve a single result in each case, instead of an array, but the
`JSONSelection` syntax would not have to change to accommodate this possibility.

If the top-level JSON input itself is an array, then the whole `JSONSelection`
will be applied to each element of that array, and the result will be an array
of those results.

Compared to dealing explicitly with hard-coded array indices, this automatic
array mapping behavior is much easier to reason about, once you get the hang of
it. If you're familiar with how arrays are handled during GraphQL execution,
it's essentially the same principle, extended to the additional syntaxes
introduced by `JSONSelection`.

### Why a string-based syntax, rather than first-class syntax?

### What about field argument syntax?

### What future `JSONSelection` syntax is under consideration?
