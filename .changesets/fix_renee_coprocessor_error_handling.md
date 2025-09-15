### Improve error handling for coprocessor responses ([PR #7607](https://github.com/apollographql/router/pull/7607))

When a coprocessor returns invalid data, the router now returns an HTTP 500 with extension code `EXTERNAL_CALL_ERROR` instead of `INTERNAL_SERVER_ERROR`, and a slightly improved error message that mentions the coprocessor.

Additionally, there is less potential for errors because unused pieces are not deserialized from a coprocessor response.
In your coprocessor, we recommend _not_ sending "queryPlan" and "sdl" keys back in the response, because they are not used.

By [@goto-bus-stop](https://github.com/goto-bus-stop) in https://github.com/apollographql/router/pull/7607