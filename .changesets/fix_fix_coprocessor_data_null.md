### Fix Parsing of Coprocessor GraphQL Responses ([PR #7141](https://github.com/apollographql/router/pull/7141))

Previously Router ignored `data: null` property inside GraphQL response returned by coprocessor.
According to [GraphQL Spectification](https://spec.graphql.org/draft/#sel-FAPHLJCAACEBxlY):

> If an error was raised during the execution that prevented a valid response, the "data" entry in the response should be null.

That means if coprocessor returned valid execution error, for example:

```json
{
  "data": null,
  "errors": [{ "message": "Some execution error" }]
}
```

Router violated above restriction from GraphQL Specification by returning following response to client:

```json
{
  "errors": [{ "message": "Some execution error" }]
}
```

This fix ensures full compliance with the GraphQL specification by preserving the complete structure of error responses from coprocessors.

Contributed by [@IvanGoncharov](https://github.com/IvanGoncharov) in [#7141](https://github.com/apollographql/router/pull/7141)
