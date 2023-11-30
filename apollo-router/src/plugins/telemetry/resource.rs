use std::collections::BTreeMap;
use std::env;
use std::time::Duration;

use opentelemetry::sdk::resource::EnvResourceDetector;
use opentelemetry::sdk::resource::ResourceDetector;
use opentelemetry::sdk::Resource;
use opentelemetry::KeyValue;

use crate::plugins::telemetry::config::AttributeValue;
const UNKNOWN_SERVICE: &str = "unknown_service";
const OTEL_SERVICE_NAME: &str = "OTEL_SERVICE_NAME";

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

pub(crate) trait ConfigResource {
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
                Box::new(config_resource_detector),
                Box::new(EnvResourceDetector::new()),
                Box::new(EnvServiceNameDetector),
            ],
        );

        // Default service name
        if resource
            .get(opentelemetry_semantic_conventions::resource::SERVICE_NAME)
            .is_none()
        {
            let executable_name = executable_name();
            resource.merge(&Resource::new(vec![KeyValue::new(
                opentelemetry_semantic_conventions::resource::SERVICE_NAME,
                executable_name
                    .map(|executable_name| format!("{}:{}", UNKNOWN_SERVICE, executable_name))
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

        // Add any other resources from config
        for (key, value) in self.resources.iter() {
            config_resources.push(KeyValue::new(key.clone(), value.clone()));
        }

        // Some other basic resources
        config_resources.push(KeyValue::new(
            opentelemetry_semantic_conventions::resource::SERVICE_VERSION,
            std::env!("CARGO_PKG_VERSION"),
        ));
        if let Some(executable_name) = executable_name() {
            config_resources.push(KeyValue::new(
                opentelemetry_semantic_conventions::resource::PROCESS_EXECUTABLE_NAME,
                executable_name,
            ));
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
                .get(&opentelemetry_semantic_conventions::resource::SERVICE_NAME.to_string())
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

#[cfg(test)]
mod test {
    use std::collections::BTreeMap;
    use std::env;

    use opentelemetry::Key;
    use serial_test::serial;

    use crate::plugins::telemetry::config::AttributeValue;
    use crate::plugins::telemetry::resource::ConfigResource;

    struct TestConfig {
        service_name: Option<String>,
        service_namespace: Option<String>,
        resources: BTreeMap<String, AttributeValue>,
    }
    impl ConfigResource for TestConfig {
        fn service_name(&self) -> &Option<String> {
            &self.service_name
        }
        fn service_namespace(&self) -> &Option<String> {
            &self.service_namespace
        }
        fn resource(&self) -> &BTreeMap<String, AttributeValue> {
            &self.resources
        }
    }

    // All of the tests in this module must execute serially wrt each other because they rely on
    // env settings and one of the tests modifies the env for the duration of the test. We enforce
    // this with the #[serial] derive.
    #[test]
    #[serial]
    fn test_empty() {
        let test_config = TestConfig {
            service_name: None,
            service_namespace: None,
            resources: Default::default(),
        };
        let resource = test_config.to_resource();
        assert!(resource
            .get(opentelemetry_semantic_conventions::resource::SERVICE_NAME)
            .unwrap()
            .as_str()
            .starts_with("unknown_service:apollo_router"));
        assert!(resource
            .get(opentelemetry_semantic_conventions::resource::SERVICE_NAMESPACE)
            .is_none());
        assert_eq!(
            resource.get(opentelemetry_semantic_conventions::resource::SERVICE_VERSION),
            Some(std::env!("CARGO_PKG_VERSION").into())
        );

        assert!(resource
            .get(opentelemetry_semantic_conventions::resource::PROCESS_EXECUTABLE_NAME)
            .expect("expected excutable name")
            .as_str()
            .contains("apollo"));
    }

    #[test]
    #[serial]
    fn test_config_resources() {
        let test_config = TestConfig {
            service_name: None,
            service_namespace: None,
            resources: BTreeMap::from_iter(vec![
                (
                    opentelemetry_semantic_conventions::resource::SERVICE_NAME.to_string(),
                    AttributeValue::String("override-service-name".to_string()),
                ),
                (
                    opentelemetry_semantic_conventions::resource::SERVICE_NAMESPACE.to_string(),
                    AttributeValue::String("override-namespace".to_string()),
                ),
                (
                    "extra-key".to_string(),
                    AttributeValue::String("extra-value".to_string()),
                ),
            ]),
        };
        let resource = test_config.to_resource();
        assert_eq!(
            resource.get(opentelemetry_semantic_conventions::resource::SERVICE_NAME),
            Some("override-service-name".into())
        );
        assert_eq!(
            resource.get(opentelemetry_semantic_conventions::resource::SERVICE_NAMESPACE),
            Some("override-namespace".into())
        );
        assert_eq!(
            resource.get(Key::from_static_str("extra-key")),
            Some("extra-value".into())
        );
    }

    #[test]
    #[serial]
    fn test_service_name_service_namespace() {
        let test_config = TestConfig {
            service_name: Some("override-service-name".to_string()),
            service_namespace: Some("override-namespace".to_string()),
            resources: BTreeMap::new(),
        };
        let resource = test_config.to_resource();
        assert_eq!(
            resource.get(opentelemetry_semantic_conventions::resource::SERVICE_NAME),
            Some("override-service-name".into())
        );
        assert_eq!(
            resource.get(opentelemetry_semantic_conventions::resource::SERVICE_NAMESPACE),
            Some("override-namespace".into())
        );
    }

    #[test]
    #[serial]
    fn test_service_name_override() {
        // Order of precedence
        // OTEL_SERVICE_NAME env
        // OTEL_RESOURCE_ATTRIBUTES env
        // config service_name
        // config resources
        // unknown_service:executable_name
        // unknown_service (Untested as it can't happen)

        assert!(TestConfig {
            service_name: None,
            service_namespace: None,
            resources: Default::default(),
        }
        .to_resource()
        .get(opentelemetry_semantic_conventions::resource::SERVICE_NAME)
        .unwrap()
        .as_str()
        .starts_with("unknown_service:apollo_router"));

        assert_eq!(
            TestConfig {
                service_name: None,
                service_namespace: None,
                resources: BTreeMap::from_iter(vec![(
                    opentelemetry_semantic_conventions::resource::SERVICE_NAME.to_string(),
                    AttributeValue::String("yaml-resource".to_string()),
                )]),
            }
            .to_resource()
            .get(opentelemetry_semantic_conventions::resource::SERVICE_NAME),
            Some("yaml-resource".into())
        );

        assert_eq!(
            TestConfig {
                service_name: Some("yaml-service-name".to_string()),
                service_namespace: None,
                resources: BTreeMap::from_iter(vec![(
                    opentelemetry_semantic_conventions::resource::SERVICE_NAME.to_string(),
                    AttributeValue::String("yaml-resource".to_string()),
                )]),
            }
            .to_resource()
            .get(opentelemetry_semantic_conventions::resource::SERVICE_NAME),
            Some("yaml-service-name".into())
        );

        env::set_var("OTEL_RESOURCE_ATTRIBUTES", "service.name=env-resource");
        assert_eq!(
            TestConfig {
                service_name: Some("yaml-service-name".to_string()),
                service_namespace: None,
                resources: BTreeMap::from_iter(vec![(
                    opentelemetry_semantic_conventions::resource::SERVICE_NAME.to_string(),
                    AttributeValue::String("yaml-resource".to_string()),
                )]),
            }
            .to_resource()
            .get(opentelemetry_semantic_conventions::resource::SERVICE_NAME),
            Some("env-resource".into())
        );

        env::set_var("OTEL_SERVICE_NAME", "env-service-name");
        assert_eq!(
            TestConfig {
                service_name: Some("yaml-service-name".to_string()),
                service_namespace: None,
                resources: BTreeMap::from_iter(vec![(
                    opentelemetry_semantic_conventions::resource::SERVICE_NAME.to_string(),
                    AttributeValue::String("yaml-resource".to_string()),
                )]),
            }
            .to_resource()
            .get(opentelemetry_semantic_conventions::resource::SERVICE_NAME),
            Some("env-service-name".into())
        );

        env::remove_var("OTEL_SERVICE_NAME");
        env::remove_var("OTEL_RESOURCE_ATTRIBUTES");
    }
}
