### Fix crash when an invalid query plan is generated ([PR #7214](https://github.com/apollographql/router/pull/7214))

When an invalid query plan is generated, the router could panic and crash.
This could happen if there are gaps in the GraphQL validation implementation.
Now, even if there are unresolved gaps, the router will handle it gracefully and reject the request.

By [@goto-bus-stop](https://github.com/goto-bus-stop) in https://github.com/apollographql/router/pull/7214