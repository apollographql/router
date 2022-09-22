# Changelog for the next release

All notable changes to Router will be documented in this file.

This project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

<!-- <THIS IS AN EXAMPLE, DO NOT REMOVE>

# [x.x.x] (unreleased) - 2022-mm-dd
> Important: X breaking changes below, indicated by **❗ BREAKING ❗**
## ❗ BREAKING ❗
## 🚀 Features
## 🐛 Fixes
## 🛠 Maintenance
## 📚 Documentation

## Example section entry format

### Headline ([Issue #ISSUE_NUMBER](https://github.com/apollographql/router/issues/ISSUE_NUMBER))

Description! And a link to a [reference](http://url)

By [@USERNAME](https://github.com/USERNAME) in https://github.com/apollographql/router/pull/PULL_NUMBER
-->

# [x.x.x] (unreleased) - 2022-mm-dd

## ❗ BREAKING ❗

### Removed `Request::from_bytes()` from public API ([Issue #1855](https://github.com/apollographql/router/issues/1855))

We've removed `Request::from_bytes()` from the public API.  We were no longer using it internally and we hardly expect anyone external to have been relying on it so it was worth the remaining breaking change prior to v1.0.0.

We discovered this function during an exercise of documenting our entire public API.  While we considered keeping it, it didn't necessarily meet our requirements for shipping it in the public API.  It's internal usage was removed in [`d147f97d`](https://github.com/apollographql/router/commit/d147f97d as part of [PR #429](https://github.com/apollographql/router/pull/429).

We're happy to consider re-introducing this in the future (it even has a matching `Response::from_bytes()` which it composes against nicely!), but we thought it was best to remove it for the time-being.

By [@abernix](https://github.com/abernix) in https://github.com/apollographql/router/pull/1858

## 🚀 Features

### Reintroduce health check ([Issue #1861](https://github.com/apollographql/router/issues/1861))

We have re-introduced a health check at the `/health` endpoint on a dedicated port that is not exposed on the default GraphQL execution port (`4000`) but instead on port `8088`.  This health check endpoint will act as an "overall" health check for the Router and we intend to add separate "liveliness" and "readiness" checks on their own dedicated endpoints (e.g., `/health/live` and `/health/ready`) in the future.  At that time, this root `/health` check will aggregate all other health checks to provide an overall health status however, today, it is simply a "liveliness" check and we have not defined "readiness".  We also intend to use port `8088` for other ("internal") functionality in the future, keeping the GraphQL execution endpoint dedicated to serving external client requests.

As for some additional context as to why we've brought it back so quickly: We had previously removed the health check we had been offering in [PR #1766](https://github.com/apollographql/router/pull/1766) because we wanted to do some additional configurationd design and lean into a new "admin port" (`8088`).  As a temporary solution, we offered the instruction to send a `GET` query to the Router with a GraphQL query.  After some new learnings and feedback, we've had to re-visit that conversation earlier than we expected!

Due to [default CSRF protections](https://www.apollographql.com/docs/router/configuration/csrf/) enabled in the Router, `GET` requests need to be accompanied by certain HTTP headers in order to disqualify them as being [CORS-preflightable](https://developer.mozilla.org/en-US/docs/Glossary/Preflight_request) requests.  While sending the additional header was reasonable straightforward in Kubernetes, other environments (including Google Kubernetes Engine's managed load balancers) didn't offer the ability to send those necessary HTTP headers along with their `GET` queries.  So, the `/health` endpoint is back.

The health check endpoint is now exposed on `127.0.0.1:8088/health` by default, and its `listen` socket address can be changed in the YAML configuration:

```yaml
health-check:
  listen: 127.0.0.1:8088 # default
  enabled: true # default
```

The previous health-check suggestion (i.e., `GET /?query={__typename}`) will still work, so long as your infrastructure supports sending custom HTTP headers with HTTP requests.

By [@o0Ignition0o](https://github.com/o0Ignition0o) in https://github.com/apollographql/router/pull/1859

## 🐛 Fixes

### Remove `apollo_private` and OpenTelemetry entries from logs ([Issue #1862](https://github.com/apollographql/router/issues/1862))

This change removes some `apollo_private` and OpenTelemetry (e.g., `otel.kind`) fields from the logs.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/1868

### Update and validate `Dockerfile` files ([Issue #1854](https://github.com/apollographql/router/issues/1854))

Several of the `Dockerfile`s in the repository were out-of-date with respect to recent configuration changes.  We've updated the configuration files and extended our tests to catch this automatically in the future.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/1857

## 🛠 Maintenance

### Disable Deno snapshotting when building inside `docs.rs`

This works around [V8 linking errors](https://docs.rs/crate/apollo-router/1.0.0-rc.2/builds/633287) and caters to specific build-environment constraints and requirements that exist on the Rust documentation site `docs.rs`.

By [@SimonSapin](https://github.com/SimonSapin) in https://github.com/apollographql/router/pull/1847

### Add the Studio Uplink schema to the repository, with a test checking that it is up to date.

Previously we were downloading the Apollo Studio Uplink schema (which is used for fetching Managed Federation schema updates) at compile-time, which would [fail](https://docs.rs/crate/lets-see-if-this-builds-on-docs-rs/0.0.1/builds/633305) in build environments without Internet access, like `docs.rs`' build system.

If an update is needed, the test failure will print a message with the command to run.

By [@SimonSapin](https://github.com/SimonSapin) in https://github.com/apollographql/router/pull/1847

## 📚 Documentation
