### Add metrics and debug logs for license-fetch failures

The router now emits `apollo.router.license.fetch.failure.total`, a counter tagged by
`source` (`file`, `env`, `registry`, `oci`) and `reason` (e.g. `not_found`, `io_error`,
`invalid_license`, `version_incompatible`, `http_error`, `fetch_failed`, `uplink_error`,
`parse_error`, `stream_init_error`), so operators can see how often license fetching fails
and why. Debug-level logging was also added along the license-fetch and JWT-decode paths
that previously had little or no diagnostic detail.

By [@BobaFetters](https://github.com/BobaFetters) in https://github.com/apollographql/router/pull/10279
