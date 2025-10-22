//! All of the tests in this file must execute serially wrt each other because they rely on
//! env settings and one of the tests modifies the env for the duration of the test.

use std::collections::BTreeMap;
use std::env;

use apollo_router::_private::telemetry::AttributeValue;
use apollo_router::_private::telemetry::ConfigResource;
use libtest_mimic::Arguments;
use libtest_mimic::Failed;
use libtest_mimic::Trial;
use opentelemetry::Key;

fn main() {
    let mut args = Arguments::from_args();
    args.test_threads = Some(1); // Run sequentially

    let tests = vec![
        Trial::test("test_empty", test_empty),
        Trial::test("test_config_resources", test_config_resources),
        Trial::test("test_service_name_override", test_service_name_override),
        Trial::test(
            "test_service_name_service_namespace",
            test_service_name_service_namespace,
        ),
    ];
    libtest_mimic::run(&args, tests).exit();
}

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

fn test_empty() -> Result<(), Failed> {
    let test_config = TestConfig {
        service_name: None,
        service_namespace: None,
        resources: Default::default(),
    };
    let resource = test_config.to_resource();
    let service_name = resource
        .get(&Key::from_static_str(opentelemetry_semantic_conventions::resource::SERVICE_NAME))
        .unwrap();
    assert!(
        service_name
            .as_str()
            .starts_with("unknown_service:telemetry_resources-"),
        "{service_name:?}"
    );
    assert!(
        resource
            .get(&Key::from_static_str(opentelemetry_semantic_conventions::resource::SERVICE_NAMESPACE))
            .is_none()
    );
    assert_eq!(
        resource.get(&Key::from_static_str(opentelemetry_semantic_conventions::resource::SERVICE_VERSION)),
        Some(std::env!("CARGO_PKG_VERSION").into())
    );

    assert!(
        resource
            .get(&Key::from_static_str(opentelemetry_semantic_conventions::resource::PROCESS_EXECUTABLE_NAME))
            .expect("expected excutable name")
            .as_str()
            .contains("telemetry_resources")
    );
    Ok(())
}

fn test_config_resources() -> Result<(), Failed> {
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
        resource.get(&Key::from_static_str(opentelemetry_semantic_conventions::resource::SERVICE_NAME)),
        Some("override-service-name".into())
    );
    assert_eq!(
        resource.get(&Key::from_static_str(opentelemetry_semantic_conventions::resource::SERVICE_NAMESPACE)),
        Some("override-namespace".into())
    );
    assert_eq!(
        resource.get(&Key::from_static_str("extra-key")),
        Some("extra-value".into())
    );
    Ok(())
}

fn test_service_name_service_namespace() -> Result<(), Failed> {
    let test_config = TestConfig {
        service_name: Some("override-service-name".to_string()),
        service_namespace: Some("override-namespace".to_string()),
        resources: BTreeMap::new(),
    };
    let resource = test_config.to_resource();
    assert_eq!(
        resource.get(&Key::from_static_str(opentelemetry_semantic_conventions::resource::SERVICE_NAME)),
        Some("override-service-name".into())
    );
    assert_eq!(
        resource.get(&Key::from_static_str(opentelemetry_semantic_conventions::resource::SERVICE_NAMESPACE)),
        Some("override-namespace".into())
    );
    Ok(())
}

fn test_service_name_override() -> Result<(), Failed> {
    // Order of precedence
    // OTEL_SERVICE_NAME env
    // OTEL_RESOURCE_ATTRIBUTES env
    // config service_name
    // config resources
    // unknown_service:executable_name
    // unknown_service (Untested as it can't happen)

    assert!(
        TestConfig {
            service_name: None,
            service_namespace: None,
            resources: Default::default(),
        }
        .to_resource()
        .get(&Key::from_static_str(opentelemetry_semantic_conventions::resource::SERVICE_NAME))
        .unwrap()
        .as_str()
        .starts_with("unknown_service:telemetry_resources-")
    );

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
        .get(&Key::from_static_str(opentelemetry_semantic_conventions::resource::SERVICE_NAME)),
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
        .get(&Key::from_static_str(opentelemetry_semantic_conventions::resource::SERVICE_NAME)),
        Some("yaml-service-name".into())
    );

    // SAFETY: this program is single-threaded
    unsafe {
        env::set_var("OTEL_RESOURCE_ATTRIBUTES", "service.name=env-resource");
    }
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
        .get(&Key::from_static_str(opentelemetry_semantic_conventions::resource::SERVICE_NAME)),
        Some("env-resource".into())
    );

    // SAFETY: this program is single-threaded
    unsafe {
        env::set_var("OTEL_SERVICE_NAME", "env-service-name");
    }
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
        .get(&Key::from_static_str(opentelemetry_semantic_conventions::resource::SERVICE_NAME)),
        Some("env-service-name".into())
    );

    // SAFETY: this program is single-threaded
    unsafe {
        env::remove_var("OTEL_SERVICE_NAME");
        env::remove_var("OTEL_RESOURCE_ATTRIBUTES");
    }
    Ok(())
}
