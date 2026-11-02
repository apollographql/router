### Actionable diagnostics for expired and version-incompatible entitlements

Two license failure modes were previously indistinguishable from any other failure in the
logs. When a license's format is one this router version can't understand (a required
claim is missing, or a claim doesn't deserialize into the expected shape — most likely
because the license is newer or older than what this router supports), it was logged
identically to a corrupt or improperly signed license, with no hint that upgrading the
router might be the fix. `apollo.router.uplink.license_enforcement::Error` now exposes
`is_version_incompatible()`, and every license source (file, env var, and Apollo Uplink)
logs this case under its own `APOLLO_ROUTER_LICENSE_VERSION_INCOMPATIBLE` code with a
message suggesting a router upgrade, instead of the generic invalid-license code.

Separately, when a license expires and the router transitions into its halted or warning
state, that transition is now logged once immediately (`APOLLO_ROUTER_LICENSE_EXPIRED`),
in addition to the existing rate-limited log that only fires once a request is actually
served while halted. A router that stops receiving traffic right as its license expires
previously logged nothing explaining why — indistinguishable from a crash.

By [@BobaFetters](https://github.com/BobaFetters) in https://github.com/apollographql/router/pull/10241
