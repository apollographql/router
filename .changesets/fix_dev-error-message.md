### Improved messaging when a request is received without an operation ([Issue #2941](https://github.com/apollographql/router/issues/2941))

The message that is displayed when a request has been sent to the Router without an operation has been improved.  This materializes as a developer experience improvement since users (especially those using GraphqL for the first time) might send a request to the Router using a tool that isn't GraphQL-aware, or might just have their API tool of choice misconfigured.

Previously, the message stated "missing query string", but now more helpfully suggests sending either a POST or GET request and specifying the desired operation as the `query` parameter (i.e., either in the POST data or in the query string parameters for GET queries).

By [@kushal-93](https://github.com/kushal-93) in https://github.com/apollographql/router/pull/2955
