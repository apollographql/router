use std::collections::BTreeMap;
use std::env;
use std::time::Duration;

use opentelemetry::KeyValue;
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::resource::EnvResourceDetector;
use opentelemetry_sdk::resource::ResourceDetector;

use crate::plugins::telemetry::config::AttributeValue;
const UNKNOWN_SERVICE: &str = "unknown_service";
const OTEL_SERVICE_NAME: &str = "OTEL_SERVICE_NAME";

/// This resource detector fills out things like the default service version and executable name.
/// Users can always override them via config.
struct StaticResourceDetector;
impl ResourceDetector for StaticResourceDetector {
    fn detect(&self, _timeout: Duration) -> Resource {
        let mut config_resources = vec![];
        config_resources.push(KeyValue::new(
            opentelemetry_semantic_conventions::resource::SERVICE_VERSION,
            std::env!("CARGO_PKG_VERSION"),
        ));

        // Some other basic resources
        if let Some(executable_name) = executable_name() {
            config_resources.push(KeyValue::new(
                opentelemetry_semantic_conventions::resource::PROCESS_EXECUTABLE_NAME,
                executable_name,
            ));
        }
        Resource::new(config_resources)
    }
}

struct EnvServiceNameDetector;
// Used instead of SdkProvidedResourceDetector
impl ResourceDetector for EnvServiceNameDetector {
    fn detect(&self, _timeout: Duration) -> Resource {
        match env::var(OTEL_SERVICE_NAME) {
            Ok(service_name) if !service_name.is_empty() => Resource::new(vec![KeyValue::new(
                opentelemetry_semantic_conventions::resource::SERVICE_NAME,
                service_name,
            )]),
            Ok(_) | Err(_) => Resource::new(vec![]), // return empty resource
        }
    }
}

#[allow(missing_docs)] // only public-but-hidden for tests
pub trait ConfigResource {
    fn service_name(&self) -> &Option<String>;
    fn service_namespace(&self) -> &Option<String>;

    fn resource(&self) -> &BTreeMap<String, AttributeValue>;

    fn to_resource(&self) -> Resource {
        let config_resource_detector = ConfigResourceDetector {
            service_name: self.service_name().clone(),
            service_namespace: self.service_namespace().clone(),
            resources: self.resource().clone(),
        };

        // Last one wins
        let resource = Resource::from_detectors(
            Duration::from_secs(0),
            vec![
                Box::new(StaticResourceDetector),
                Box::new(config_resource_detector),
                Box::new(EnvResourceDetector::new()),
                Box::new(EnvServiceNameDetector),
            ],
        );

        // Default service name
        if resource
            .get(opentelemetry_semantic_conventions::resource::SERVICE_NAME.into())
            .is_none()
        {
            let executable_name = executable_name();
            resource.merge(&Resource::new(vec![KeyValue::new(
                opentelemetry_semantic_conventions::resource::SERVICE_NAME,
                executable_name
                    .map(|executable_name| format!("{UNKNOWN_SERVICE}:{executable_name}"))
                    .unwrap_or_else(|| UNKNOWN_SERVICE.to_string()),
            )]))
        } else {
            resource
        }
    }
}

fn executable_name() -> Option<String> {
    std::env::current_exe().ok().and_then(|path| {
        path.file_name()
            .and_then(|p| p.to_str().map(|s| s.to_string()))
    })
}

struct ConfigResourceDetector {
    service_name: Option<String>,
    service_namespace: Option<String>,
    resources: BTreeMap<String, AttributeValue>,
}

impl ResourceDetector for ConfigResourceDetector {
    fn detect(&self, _timeout: Duration) -> Resource {
        let mut config_resources = vec![];

        // For config resources last entry wins
        for (key, value) in self.resources.iter() {
            config_resources.push(KeyValue::new(key.clone(), value.clone()));
        }

        // Service namespace
        if let Some(service_namespace) = self.service_namespace.clone() {
            config_resources.push(KeyValue::new(
                opentelemetry_semantic_conventions::resource::SERVICE_NAMESPACE,
                service_namespace.to_string(),
            ));
        }

        if let Some(service_name) = self.service_name.clone().or_else(|| {
            // Yaml resources
            if let Some(AttributeValue::String(name)) = self
                .resources
                .get(opentelemetry_semantic_conventions::resource::SERVICE_NAME)
            {
                Some(name.clone())
            } else {
                None
            }
        }) {
            config_resources.push(KeyValue::new(
                opentelemetry_semantic_conventions::resource::SERVICE_NAME,
                service_name.to_string(),
            ));
        }
        Resource::new(config_resources)
    }
}

// Tests in apollo-router/tests/telemetry_resource_tests.rs
