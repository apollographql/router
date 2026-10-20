### Reject deeply nested connector selections instead of crashing

A deeply nested `@connect(selection:)` could overflow the stack and abort composition or router startup. It is now rejected with a `Selection nested too deeply` validation error.

By [@tninesling](https://github.com/tninesling) in https://github.com/apollographql/router/pull/10584
