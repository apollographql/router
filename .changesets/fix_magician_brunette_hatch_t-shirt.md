### Fix authorization plugin handling of polymorphic types

Updates the authorization plugin to correctly handle authorization requirements when processing polymorphic types.

When querying interface fields, the authorization plugin was verifying only whether all implementations shared the same
authorization requirements. In cases where interface did not specify any authorization requirements, this could result in
unauthorized access to protected data.

The authorization plugin was updated to correctly verify that all polymorphic authorization requirements are satisfied by
the current context.

By [@dariuszkuc](https://github.com/dariuszkuc) in https://github.com/apollographql/router/pull/PULL_NUMBER