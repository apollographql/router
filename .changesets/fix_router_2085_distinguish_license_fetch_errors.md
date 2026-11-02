### Distinguish "not found" from transient failures when fetching an entitlement from an OCI registry

The OCI graph-artifact entitlement fetch previously logged every failure — a missing manifest, a missing entitlement layer, an auth failure, a 5xx, or a network error — identically as an error, with no way to tell "this router genuinely has no license" apart from a transient failure that should just be retried. `OciError` now exposes an `is_not_found()` classification, and the entitlement fetch logs accordingly: a not-found is logged at debug level as an expected outcome, while a transient failure is logged as a warning noting it will be retried. This paves the way for a future change to only treat a genuine not-found as "unlicensed," so a transient blip can no longer be mistaken for a router losing its license.

By [@BobaFetters](https://github.com/BobaFetters) in https://github.com/apollographql/router/pull/####
