# Title [ADR-1]

Collecting structured, event based diagnostics (Telemetry)

## Status

Proposed

## Context

The project uses [tracing](https://docs.rs/tracing/latest/tracing/) to
collect this information. The tracing crate provides the helpful
"instrument" macro for creating/entering tracing spans on function
invoke.

Currently, our use of this macro is undifferentiated and we are seeing
issues where our telemetry data is affecting performance of the router
because of volumes and format (JSON) of collected data.

## Decision

This ADR proposes that we change our approach to using #[instrument]
so that we only trace the exact information which we need. This
will require a review of existing usage.

Recommendations:
 - Always specify `skip_all` when using the #[instrument] macro

e.g.:
```
#[instrument(skip_all, ...)]
```

This will have the effect of making the macro "opt-in" rather than the
default behaviour of "opt-out" and will make mistakes easier to spot
and prevent in code review. Required fields can be specified using the
"fields" attribute.

 - Ensure that each instrumented function is named to promote understanding
   and consistency

 - Ensure that instrumentation is at the "info" level. Other levels, such
   as "debug" or "trace" should be avoided if possible and strictly
   reserved for developer problem solving.

 - Document the agreed upon standard usage in DEVELOPMENT.md and ensure
   that the standard is maintained via code review and tooling.

## Consequences

It should be much simpler for clients to consume diagnostics.

We will be transmitting much less data via telemetry and avoiding sharing
confidential data.

Telemetry stability is promoted.

If a developer requires access to data that was previously offered in
telemetry it is simple to make a temporary change or consume logs with
the required data.
