### add errors for response validation ([Issue #5372](https://github.com/apollographql/router/issues/5372))

When formatting responses, the router is validating the data returned by subgraphs and replacing it with null values as appropriate. That validation phase is now adding errors when encountering the wrong type in a field requested by the client.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/5787