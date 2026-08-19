//! Legacy → UCUM rename table for ROUTER-1777.
//!
//! Each entry maps a legacy OTel metric name (currently emitted via a deprecated
//! non-unit macro) to the OTel name + UCUM unit it will canonically use going
//! forward. During this router 3.x dual-emit window, callsites whose name is in
//! this table emit a secondary instrument on the `apollo/router/ucum` meter scope
//! so customer dashboards can migrate to the suffixed Prometheus name before the
//! legacy name is removed in a future major version.
//!
//! Scope: the `opentelemetry-prometheus` exporter only appends a Prometheus-name
//! suffix when the OTel `unit` is a physical UCUM unit (e.g. `s`, `By`). Metrics
//! whose future unit is an annotation (e.g. `{request}`, `{event}`) don't change
//! their Prometheus name when migrated to `_with_unit!`, so they don't need
//! dual-emit and are intentionally absent from this table.

/// Looks up the (new OTel name, UCUM unit) for a legacy metric name.
///
/// Returns `None` if the name should not be dual-emitted.
pub(crate) fn rename_for(legacy_name: &str) -> Option<(&'static str, &'static str)> {
    let pair = match legacy_name {
        // Durations → unit "s" → Prom appends "_seconds"
        "apollo.router.cache.hit.time" => ("apollo.router.cache.hit.time", "s"),
        "apollo.router.cache.invalidation.duration" => {
            ("apollo.router.cache.invalidation.duration", "s")
        }
        "apollo.router.cache.miss.time" => ("apollo.router.cache.miss.time", "s"),
        "apollo.router.operations.coprocessor.duration" => {
            ("apollo.router.operations.coprocessor.duration", "s")
        }
        "apollo.router.query_planning.plan.duration" => {
            ("apollo.router.query_planning.plan.duration", "s")
        }
        "apollo.router.query_planning.total.duration" => {
            ("apollo.router.query_planning.total.duration", "s")
        }
        "apollo.router.query_planning.warmup.duration" => {
            ("apollo.router.query_planning.warmup.duration", "s")
        }
        "apollo.router.schema.load.duration" => ("apollo.router.schema.load.duration", "s"),
        // The legacy OTel name encodes the unit in the name; drop the trailing
        // `.seconds` so the new instrument has clean OTel semantics. The Prom
        // output happens to land on the same string as the legacy.
        "apollo.router.uplink.fetch.duration.seconds" => {
            ("apollo.router.uplink.fetch.duration", "s")
        }

        // Bytes → unit "By" → Prom appends "_bytes"
        "apollo.router.operations.fetch.request_size" => {
            ("apollo.router.operations.fetch.request_size", "By")
        }
        "apollo.router.operations.fetch.response_size" => {
            ("apollo.router.operations.fetch.response_size", "By")
        }
        "apollo.router.operations.request_size" => ("apollo.router.operations.request_size", "By"),
        "apollo.router.operations.response_size" => {
            ("apollo.router.operations.response_size", "By")
        }
        "apollo.router.operations.file_uploads.file_size" => {
            ("apollo.router.operations.file_uploads.file_size", "By")
        }

        _ => return None,
    };
    Some(pair)
}

/// Meter scope used for the secondary (UCUM-suffixed) emissions. Distinct from
/// the legacy `apollo/router` scope so OTel SDK accepts both instruments and
/// Prometheus output labels them with different `otel_scope_name`.
pub(crate) const UCUM_METER_SCOPE: &str = "apollo/router/ucum";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_legacy_name_resolves() {
        assert_eq!(
            rename_for("apollo.router.cache.hit.time"),
            Some(("apollo.router.cache.hit.time", "s"))
        );
    }

    #[test]
    fn uplink_drops_seconds_suffix() {
        assert_eq!(
            rename_for("apollo.router.uplink.fetch.duration.seconds"),
            Some(("apollo.router.uplink.fetch.duration", "s"))
        );
    }

    #[test]
    fn byte_metric_resolves() {
        assert_eq!(
            rename_for("apollo.router.operations.file_uploads.file_size"),
            Some(("apollo.router.operations.file_uploads.file_size", "By"))
        );
    }

    #[test]
    fn unknown_name_returns_none() {
        assert_eq!(rename_for("apollo.router.operations"), None);
        assert_eq!(rename_for("apollo.router.graphql_error"), None);
        assert_eq!(rename_for(""), None);
    }
}
