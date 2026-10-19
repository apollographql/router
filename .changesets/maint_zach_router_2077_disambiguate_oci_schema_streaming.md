### Disambiguate OCI schema-streaming functions from license-streaming functions

In `apollo-router/src/registry/mod.rs`, the functions that stream the schema from the OCI registry predate their license/entitlement counterparts and reused the same naming pattern, so they didn't say "schema" anywhere. The later license-streaming functions do say "license" (`fetch_license_oci`, `stream_license_from_oci`, `fetch_license_from_reference`), which made the two families hard to tell apart at a glance.

`fetch_oci_from_reference`, `fetch_oci`, and `stream_from_oci` (all private or `pub(crate)`, with no external callers other than the already correctly-named `create_oci_schema_stream`) are renamed to `fetch_schema_from_reference`, `fetch_schema_oci`, and `stream_schema_from_oci` respectively. This is a pure rename with no behavior change.

By [@BobaFetters](https://github.com/BobaFetters) in https://github.com/apollographql/router/pull/####
