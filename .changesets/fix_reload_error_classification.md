### Stop retrying schema, configuration, and license reloads that can never succeed ([PR #10000](https://github.com/apollographql/router/pull/10000))

When the router hot reloads — because a new schema or license arrived from GraphOS, or because its configuration file changed — it now distinguishes failures that a retry might fix from ones it cannot. A schema that fails to parse, a configuration that violates license enforcement, or a schema using preview features that configuration hasn't enabled will fail in exactly the same way on every attempt, so the router no longer retries them on a timer — it keeps serving the last good state and waits for new inputs instead. Failures that could plausibly clear on their own, such as a plugin failing to reach a dependency while pipelines are being built, are retried as before.

This only changes what happens on the retry timer. Whatever the failure, the router keeps serving the previously committed schema, configuration and license, and the next publish always gets an immediate attempt with a fresh retry budget.

A new metric, `apollo.router.state.reload.attempt`, counts reload attempts. Its `is_success` attribute records whether the attempt built, and `error_kind` records why it didn't: `transient`, `permanent`, or `fatal`.

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/10000
