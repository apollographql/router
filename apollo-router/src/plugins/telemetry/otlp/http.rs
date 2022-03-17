use super::ExportConfig;
use crate::configuration::ConfigurationError;
use opentelemetry_otlp::WithExportConfig;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Deserialize, Serialize, Default, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HttpExporter {
    #[serde(flatten)]
    export_config: ExportConfig,
    headers: Option<HashMap<String, String>>,
}

impl HttpExporter {
    pub fn exporter(&self) -> Result<opentelemetry_otlp::HttpExporterBuilder, ConfigurationError> {
        let mut exporter = opentelemetry_otlp::new_exporter().http();
        exporter = self.export_config.apply(exporter);
        if let Some(headers) = self.headers.clone() {
            exporter = exporter.with_headers(headers);
        }
        Ok(exporter)
    }

    pub fn exporter_from_env() -> opentelemetry_otlp::HttpExporterBuilder {
        let mut exporter = opentelemetry_otlp::new_exporter().http();
        exporter = exporter.with_env();
        exporter
    }
}
