### Actionable diagnostics for invalid or corrupted entitlements

When a license file downloads successfully but is itself broken — corrupted, in the
wrong format, or failing its signature check — it was logged under its own
`APOLLO_ROUTER_LICENSE_INVALID` code, but with only a bare `Failed to parse license: ...`
message wrapping the underlying JWT decode error. This case now logs a clear, actionable
message pointing at re-downloading the license or contacting Apollo support, matching the
pattern already used for expired and version-incompatible licenses. No detection logic
changed — `is_version_incompatible()` already correctly separates this case from a
version mismatch; only the message shown for it does.

By [@BobaFetters](https://github.com/BobaFetters) in https://github.com/apollographql/router/pull/####
