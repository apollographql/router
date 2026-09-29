# Reload Lifecycle

This document describes how the router decides what to serve, and what happens when it can't serve what it has been given. Everything here lives in `apollo-router/src/state_machine.rs` unless stated otherwise.

It is written for someone about to change reload behavior. The sharp edges at the end are the parts that are least obvious from reading the code, and the ones most likely to be re-derived painfully.

## The three inputs

The router is built from three inputs, and it needs all three before it can serve anything:

- **Configuration** — `Arc<Configuration>`, typically from a YAML file that may be watched for changes.
- **Schema** — `Arc<SchemaState>`: raw SDL plus an optional `launch_id`, typically from Uplink.
- **License** — `Arc<LicenseState>`, typically from Uplink.

Each has its own source type under `apollo-router/src/router/event/`, and each can also be supplied directly when the router is embedded rather than run from the CLI.

These arrive as independent `Event`s (`apollo-router/src/router/event/mod.rs`) on a single stream, are never versioned together, and come from unrelated sources. There is no such thing as "the config that goes with this schema" — a configuration edit is applied against whatever schema is currently committed, and vice versa. Keep this in mind before reasoning about any input as though it were part of a coordinated release.

The relevant events are `UpdateConfiguration`, `UpdateSchema` and `UpdateLicense`; their `NoMore*` counterparts, which turn a still-missing input into a startup error; `Reload` and `RhaiReload`; and `Shutdown`.

The two reload events are not equivalent, and neither matches what you might guess from the `Event` enum's doc comments:

- **`RhaiReload`** is accumulated with `force_reload: true`, so it rebuilds even though no input changed. This is the one that does what "reload" sounds like.
- **`Reload`** is accumulated with `force_reload: false`. It is produced only by `ReloadSource` on **SIGHUP** (`apollo-router/src/router/event/reload.rs`), so a SIGHUP with no changed input takes the `"no reload necessary"` path and does nothing. Its enum comment says "Artificial hot reload for chaos testing", which is stale — the chaos plugin has its own stream (`apollo-router/src/plugins/chaos/reload.rs`) that re-emits `UpdateSchema` / `UpdateConfiguration` with the last-seen values instead.

## The loop

`StateMachine::process_events` is a single loop over one stream. Each iteration does two things in order:

1. **Accumulate** — `State::accumulate_inputs`, a pure function with no I/O, merges the new input into the current state and decides whether a reload is needed.
2. **Attempt** — `State::attempt_reload`, which does all the work: parsing, enforcement, and building the router.

The loop's `tokio::select!` is `biased` with the event arm first and a retry timer second, so a queued event is preferred over a due retry. Note that the retry arm is a `sleep_until`, so during the delay window it isn't ready and events are processed regardless of the bias — the bias only decides simultaneous readiness.

## The states

- **`Startup`** — waiting for the first of each input. Holds the `listen_addresses` write guard, which is why callers asking for the listen address block until the router is live.
- **`Running`** — serving. Holds the three committed inputs plus the server handle and the router factory.
- **`Reloading`** — still serving the previous state, with a reload outstanding or a pending input that hasn't been applied. This is also, in practice, the *stuck* state: a permanent failure parks here indefinitely with the retry timer disarmed.
- **`Stopped`** / **`Errored`** — terminal.

`Debug` for `State` deliberately prints only the variant name, and that string is what feeds both the `"state machine transitioned"` log and the `apollo.router.state.change.total` metric. Renaming a variant is a breaking change to those.

## Committed vs pending

`Reloading` holds each input as a `PendingChange<T>`, which is either `Unchanged(value)` or `Changed { committed, pending }`:

- `committed()` is what the router is **serving**.
- `target()` is what the router is **trying to apply** — the pending value if there is one, otherwise the committed value.

A reload always builds from the merged target of all three: `(configuration.target(), schema.target(), license.target())`.

**A pending value is never discarded.** This is deliberate and predates the `Reloading` state — before [#9391](https://github.com/apollographql/router/pull/9391) the values were mutated in place on the `Running` state before the attempt, with the comment "we always want to retain the latest information for when we try to reload next". The rationale still holds: an input that cannot be served today may become servable when a *different* input changes. The clearest case is an operator enabling a licensed feature before their license is provisioned — the configuration is rejected, then the license arrives, and the retained configuration is what makes the combination succeed on that second event. `unrestricted_unlicensed_reload_with_config_using_restricted_features_and_license` is the test that pins this.

What #9391 changed was honesty about what is serving: before it, `Running.configuration` held the *rejected* configuration while the server served the previous one.

## What a reload costs, and where it can fail

`try_start` runs in a fixed order, and the order matters because the two halves have completely different properties.

**Pure and comparatively cheap** — no I/O at all:

1. `Schema::parse_arc` — parses and validates the supergraph, expands connectors, builds the API schema. Not trivial (it has its own histogram, `apollo.router.schema.load.duration`), and note it takes the **configuration** as an input, so a parsed schema cannot be cached against the SDL alone.
2. License enforcement — `LicenseEnforcementReport::build(..).enforce()`, a pure function of all three inputs.
3. Feature-gate enforcement — `FeatureGateEnforcementReport::build(..).check()`.

The order is forced, not incidental: the two enforcement steps are individually much cheaper than parsing, but both take the **parsed** `&Schema` — feature gates read `@link` directives off the supergraph, and license enforcement's `validate_schema` walks schema directives — so neither can run first. The one part that is schema-independent is license enforcement's `validate_configuration`, which needs only the configuration and the license-derived restrictions. Splitting that out to reject a bad configuration before parsing would only ever speed up the failure path, since anything that passes still has to be parsed.

**Expensive, and where all the I/O lives** — `router_configurator.create()`:

- Constructs the federation query planner.
- Instantiates every plugin, several of which do real network I/O in their constructors (telemetry exporters, coprocessor clients, Redis-backed caches, and the persisted-query manifest fetch, which is awaited before returning).
- **Warms the query plan cache, awaited inline**, so the whole reload blocks on it. Volume comes from `supergraph.query_planning.warmed_up_queries` (default: a third of the in-memory cache) plus the persisted query list when `persisted_queries.experimental_prewarm_query_plan_cache.on_reload` is set, which it is by default. Timed by `apollo.router.query_planning.warmup.duration`. On a reload with a populated cache this dominates everything else.

That split lines up exactly with the failure classification below, which is not a coincidence and is worth preserving.

## Failure classification

`ReloadError` is either `Transient` or `Permanent`, and `attempt_reload` uses it to decide whether to keep the retry timer running:

- **Permanent** — the failure is a property of the inputs themselves: a schema that doesn't parse, a configuration the license forbids, a schema using preview features the configuration doesn't enable. These are exactly the three pure checks. Retrying re-derives the same verdict, so the retry budget is pinned to `Some(0)` to disarm the timer, and the router waits for a new input instead of churning.
- **Transient** — anything from `create()`. It may succeed next time, so it is retried.

"Permanent" is scoped to *one triple*, not to the router. A change to any one input makes a new triple which gets its own verdict, and `accumulate_inputs` resets the retry budget on any event, so the next publish always gets an immediate fresh attempt.

There is a third outcome that is **not** part of the enum: **fatal**. Fatality is positional, not intrinsic — once `try_start` takes the previous server handle (the "point of no return"), there is no running server to fall back to, so any failure past that line ends in `Errored` regardless of how it was classified. `attempt_reload` detects this from `server_handle.is_some()`, not from the error, which is why the enum has no `Fatal` variant.

## The retry timer

- `reload.max_retries` (default 5) and `reload.retry_delay` (default 10s, plus up to 25% jitter, suppressed in tests) come from the **pending** configuration, so a configuration that changes the retry policy takes effect on the attempt that applies it.
- `retries_remaining` is initialized to `max_retries + 1`, because the initial event-triggered attempt consumes one slot. `None` means unlimited; `Some(0)` means no further *timer-driven* attempt.
- `Some(0)` has two causes that are worth distinguishing when reading logs: the budget ran out, or the last failure was permanent. The `error_kind` field on the failure log tells you which.
- `attempt_reload` always tries when called, regardless of the budget. The budget gates only the timer, so a fresh publish is never silently ignored even after retries are exhausted.

## Effective license

The license the router runs under is not always the one that was published. `LicenseEnforcementReport::enforce` returns an *effective* license:

- With no restricted features in use, any license collapses to `Licensed` (carrying its limits) — there is nothing to warn or halt about.
- With restricted features in use, `Licensed`/`LicensedWarn` without the needed entitlement, and `Unlicensed`, are violations.
- `LicensedHalt` is **not** a violation, and its halt state is preserved into the effective license. Halting is enforced by the axum `license_handler` middleware returning a canned response, not by refusing to build — so collapsing it here would silently resume serving on an expired license.

## Change notifications

After a successful reload, and only after the pipelines have rolled over:

- If the configuration changed, listeners are notified on the **previous** configuration's `Notify` channel, with a weak reference to the new configuration.
- If the schema changed, listeners are notified on the **new** configuration's `Notify` channel.

Both decisions currently key off `PendingChange::is_pending()`, i.e. "does the pending value differ from committed". That is only equivalent to "what did we just commit" because a reload always applies the full merged target. Anything that commits a partial combination has to compute these from the committed-before/committed-after diff instead, or it will terminate live subscriptions for a configuration change that never happened and publish the schema on a channel with no subscribers.

## Observability

- `apollo.router.state.change.total` — one per loop iteration, with `event`, `previous_state`, `state`.
- `apollo.router.state.reload.attempt` — one per reload attempt, with `is_success` and, on failure, `error_kind` of `transient` / `permanent` / `fatal`.
- The failure log is `"error while reloading, still running with previous configuration"`, carrying `error`, `error_kind` and `retries_remaining`.
- Uplink fetch outcomes are separate (`apollo.router.uplink.fetch.count.total`). A schema that was fetched successfully but never applied looks like a clean fetch, so these two need to be read together.

## Sharp edges

All of these are true as of writing. They are recorded because they are surprising, not because each has been reported in the field — several were found by reading the code.

**An unservable input blocks the others.** Because a reload builds from the merged target and pending values are never discarded, an input that can never be applied fails every subsequent attempt. Publish a configuration that violates enforcement, then publish a good schema, and that schema cannot be served until someone publishes another configuration. The retention half of this is deliberate (see above); the blocking half is a consequence nobody chose. Fixing it means being able to commit some combination other than the full merged target, which means committed and desired diverge persistently. See ROUTER-1970 for the analysis.

**A transiently-broken configuration blocks in the same way, and can't be detected cheaply.** A bad plugin option fails inside `create()` forever, is classified transient, exhausts its budget, and then blocks later publishes exactly like a permanent failure — except no pure check can see it coming. Worse, `accumulate_inputs` resets the budget on *any* event, so a stream of schema publishes can keep a doomed configuration's budget alive indefinitely and exhaustion may never be reached.

**The health check has no view of reload state.** A router parked in `Reloading` with the timer disarmed reports `UP` and ready. That is arguably correct — it *is* serving traffic healthily on its committed inputs, and a probe that failed here would have Kubernetes depool or restart a router that is working — so this is a visibility gap rather than a wrong answer. The point is only that `/health` is not where a stuck reload shows up: the health plugin is rebuilt on every reload and so cannot hold state across one, and it is not wired to the state machine at all. Look instead at the failure log and `apollo.router.state.reload.attempt` — though note both are events rather than levels, so neither tells you the condition is *still* true.

**A failed reload stops shutdown waiting for in-flight connections.** `try_start` takes `all_connections_stopped_signals` by value and drops them on any early return, so the failure path resets them to empty. Connections that predate a failed attempt are no longer awaited on shutdown.

**Startup has no fallback.** With nothing committed, any failure — including an unservable triple — is terminal. Several license tests depend on this, and any fallback logic must not apply here.

**SIGHUP probably doesn't do what its users expect.** As above, `Event::Reload` is accumulated with `force_reload: false`, so sending SIGHUP to a router whose inputs haven't changed reaches `"no reload necessary"` and rebuilds nothing. Whether that is intended is unclear — the conventional meaning of SIGHUP is "re-read your configuration", and `RhaiReload` sitting right next to it passes `true`. Worth confirming before relying on it either way.

## Approaches that were investigated and dropped

Both of these are ROUTER-1970. They are recorded here because the reasons they didn't work are not visible in the code, and both are the sort of thing that looks obviously correct until you look closely.

### Coalescing queued updates

The idea: when several updates are already queued, skip the intermediate ones and reload straight to the newest, so a burst of publishes doesn't pay a full query-planner warm-up per obsolete state.

There is almost nothing to coalesce. The Uplink poller is `fetch → send().await → sleep(poll_interval)` over a `channel(2)`, with the interval starting at 10s. At most three updates can exist at once — two buffered plus one held by a producer blocked in `send()` — and while a reload is warming up the poller is parked in `send()`, not polling. So the ceiling is skipping one or two builds per drain, not collapsing a queue.

The deeper problem is that Uplink is a conditional fetch of *current state* keyed on `last_id`, not a replay log. Everything sitting in the channel is therefore already stale: the "newest queued" update you would coalesce to was current when the *previous* warm-up started. Coalescing picks the least-stale of several stale options. If this is worth solving, the answer is latest-value semantics — a `watch`-style channel on the schema source, so that a reload always reads whatever is current when it starts — rather than anything that inspects the queue.

One trap if anyone revisits this: **licenses must never be coalesced.** Skipping an update is only equivalent to letting the next one supersede it if the later value unconditionally wins, and licenses don't — `accumulate_inputs` ignores an unlicensed update while the router is licensed. Coalescing a valid license sitting ahead of an unlicensed one would drop *both* and leave the router on a stale license.

### Serving the newest state that builds

The idea: since an unservable input blocks everything behind it (first sharp edge above), fall back to the newest combination that does build, so an innocent schema isn't held hostage by a broken configuration.

Three variants fail for the same underlying reason — each buys its unblocking by throwing something away:

- **Keep a list of candidate triples, and pop on failure.** Doesn't unblock anything: the newest candidate is the poisoned one, so the next publish merges into it and fails identically. Nor does the list help, since every older candidate in the reported scenario still contains the same broken input.
- **Discard an input that failed permanently.** Fixes the reported case, but reverses the deliberate retention decision described above, and breaks `unrestricted_unlicensed_restricted_licensed_with_feature_not_contained_in_allowed_features`. An operator who enables a licensed feature before their license is provisioned has their configuration thrown away; the license arriving later reloads the *old* configuration and never retries theirs.
- **Validate at accumulation time and reject the event outright.** Cheaper and tidier to implement, but semantically identical to discarding — the input never enters the state machine, so it is equally lost.

The variant that does work is to keep the newest inputs pending forever and commit a hybrid — the newest schema paired with the serving configuration and license — so that configuration and license only ever move together with what is committed, and only the schema is ever rescued. That keeps every existing test green.

It was dropped anyway, and the reason is conceptual rather than mechanical: right now what the router serves only ever moves **forward**, and every commit is the complete set of newest inputs. A fallback means committing a combination that was never published as a set and that is deliberately *older* in one input than what the router has been told about, and then holding that divergence indefinitely. That costs a real invariant, plus a rule for computing change notifications from the committed diff rather than from `is_pending()` (get it wrong and you terminate live subscriptions for a configuration change that never happened). Given the blocking behavior was found by reading the code rather than reported by anyone, that did not seem like a trade worth making yet.

What would change the calculus: an actual report of a schema being held up by a stale configuration. The narrow hybrid remains the design to reach for if that arrives.
