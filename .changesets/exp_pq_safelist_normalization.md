### Improvements to safelisting with Persisted Queries (preview)

(The Persisted Queries feature was initially released in Router v1.25.0, as part of a private preview requiring enablement by Apollo support. The feature is now in public preview and is accessible to any enterprise GraphOS organization.)

Several improvements to safelisting behavior based on preview feedback:

* When the safelist is enabled (but `require_id` is not), matching now ignores the order of top-level definitions (operations and fragments) and ignored tokens (whitespace, comments, commas, etc), so that differences in these purely syntactic elements do not affect whether an operation is considered to be in the safelist.
* If introspection is enabled on the server, any operation whose top-level fields are introspection fields (`__type`, `__schema`, or `__typename`) is considered to be in the safelist. (Previously, Router instead looked for two specific introspection queries from a particular version of Apollo Sandbox if sandbox was enabled; this hard-coded check is removed.) This special case is not applied if `require_id` is enabled, so that Router never parses freeform GraphQL in this mode.
* When `log_unknown` is enabled and `apq` has not been disabled, Router now logs any operation not in the safelist as unknown, even those sent via IDs if the operation was found in the APQ cache rather than the manifest.
* When `log_unknown` and `require_id` are both enabled, Router now logs all operations that rejects (i.e., all operations sent as freeform GraphQL). Previously, Router only logged the operations that would have been rejected by the safelist feature with `require_id` disabled (i.e., operations sent as freeform GraphQL that do not match an operation in the manifest).

As a side effect of this change, Router now re-downloads the PQ manifest when reloading configuration dynamically rather than caching it across reloads. If this causes a notable performance regression for your use case, please file an issue.

By [@glasser](https://github.com/glasser) in https://github.com/apollographql/router/pull/3566
