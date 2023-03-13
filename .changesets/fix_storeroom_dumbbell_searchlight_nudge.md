### State machine will always retain most recent config ([Issue #2752](https://github.com/apollographql/router/issues/2752))

Previously if the router failed to reload either for config or for schema changes it would discard the new information.

Now It will always retain the new information.

Changing this behaviour means that the router must enter a good configuration state before it will reload rather than reloading with potentially inconsistent state.

For example, previously:

1. Router starts with valid schema and config.
2. Router config is set to something invalid and restart doesn't happen. 
3. Router receives a new schema, the router restarts with the new schema and the original valid config.

After this change the latest information is used to restart the router always:

1. Router starts with valid schema and config.
2. Router config is set to something invalid and restart doesn't happen. 
3. Router receives a new schema, but the router fails to restart because of config is still invalid.

It is important not to reload on inconsistant states because the use of a new schema may directly rely on changes in config to work correctly.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/2753
