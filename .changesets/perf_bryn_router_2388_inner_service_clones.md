### Stop copying the request pipeline in stages that don't need it ([PR #10912](https://github.com/apollographql/router/pull/10912))

Several stages of the router's request handling copied the rest of the request pipeline on every request, then discarded the copy. Ten of them now skip the copy when they don't need it. Authorization, usage reporting and license enforcement never need it. The others copy only for the requests that do, such as subscriptions, multipart file uploads, batched requests, and calls to subgraphs with automatic persisted queries (APQ) enabled. This lowers CPU use per request.

No configuration change is needed.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/10912
