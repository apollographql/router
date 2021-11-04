use super::ExportConfig;
use crate::configuration::{ConfigurationError, Secret};
use opentelemetry_otlp::WithExportConfig;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Exporter {
    #[serde(flatten)]
    export_config: ExportConfig,
    // TODO this should probably use TlsConfig instead?
    cert: Option<Secret>,
    key: Option<Secret>,
    tls: Option<bool>,
    headers: Option<HashMap<String, String>>,
    compression: Option<opentelemetry_otlp::Compression>,
    completion_queue_count: Option<usize>,
}

impl Exporter {
    pub fn exporter(
        &self,
    ) -> Result<opentelemetry_otlp::GrpcioExporterBuilder, ConfigurationError> {
        let mut exporter = opentelemetry_otlp::new_exporter().grpcio();
        exporter = self.export_config.apply(exporter);
        match (self.cert.as_ref(), self.key.as_ref()) {
            (Some(cert), Some(key)) => {
                exporter = exporter.with_credentials(opentelemetry_otlp::Credentials {
                    cert: cert.read()?,
                    key: key.read()?,
                });
            }
            _ => {}
        }
        if let Some(headers) = self.headers.clone() {
            exporter = exporter.with_headers(headers);
        }
        if let Some(compression) = self.compression {
            exporter = exporter.with_compression(compression);
        }
        if let Some(tls) = self.tls {
            exporter = exporter.with_tls(tls);
        }
        if let Some(completion_queue_count) = self.completion_queue_count {
            exporter = exporter.with_completion_queue_count(completion_queue_count);
        }
        Ok(exporter)
    }

    pub fn exporter_from_env() -> opentelemetry_otlp::GrpcioExporterBuilder {
        let mut exporter = opentelemetry_otlp::new_exporter().grpcio();
        exporter = exporter.with_env();
        exporter
    }
}
