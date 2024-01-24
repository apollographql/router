### Create a trace during router creation and plugin initialization ([Issue #4472](https://github.com/apollographql/router/issues/4472))

When the router starts or reload, it will now generate a trace with spans for query planner creation, schema parsing, plugin initialization and request pipeline creation. This will help in debugging any issue during startup, especially in plugins creation

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/4480