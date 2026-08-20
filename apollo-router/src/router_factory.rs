use std::io;
use std::sync::Arc;

use axum::response::IntoResponse;
use http::StatusCode;
use multimap::MultiMap;
use rustls::RootCertStore;
use rustls::pki_types::CertificateDer;
use tower::BoxError;
use tower::ServiceExt;
use tower::service_fn;
use tracing::Instrument;

use crate::ListenAddr;
use crate::configuration::Configuration;
use crate::configuration::ConfigurationError;
use crate::configuration::TlsClient;
use crate::plugin::DynPlugin;
use crate::plugin::Handler;
use crate::services::router;
use crate::services::router::pipeline_handle::PipelineHandle;
use crate::services::router::service::RouterCreator;
use crate::spec::Schema;
use crate::uplink::license_enforcement::LicenseState;

pub(crate) const STARTING_SPAN_NAME: &str = "starting";

#[derive(Clone)]
/// A path and a handler to be exposed as a web_endpoint for plugins
pub struct Endpoint {
    pub(crate) path: String,
    // Plugins need to be Send + Sync
    // BoxCloneService isn't enough
    handler: EndpointHandler,
}

#[derive(Clone)]
enum EndpointHandler {
    /// Legacy handler wrapping a router service
    Service(Handler),
    /// Direct axum router (bypasses service conversion)
    Router(axum::Router),
}

impl std::fmt::Debug for Endpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Endpoint")
            .field("path", &self.path)
            .finish()
    }
}

impl Endpoint {
    /// Creates an Endpoint given a path and a Boxed Service
    pub fn from_router_service(path: String, handler: router::BoxCloneService) -> Self {
        Self {
            path,
            handler: EndpointHandler::Service(Handler::new(handler)),
        }
    }

    /// Creates an Endpoint given a path and an axum Router
    ///
    /// This is the preferred method for plugins that use axum internally,
    /// as it avoids unnecessary service wrapping and path manipulation.
    ///
    /// The router will be automatically nested at the specified path, allowing
    /// it to handle all sub-routes. For example, a router registered at `/diagnostics`
    /// will handle `/diagnostics/`, `/diagnostics/memory/status`, etc.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use axum::{Router, routing::get};
    ///
    /// let router = Router::new()
    ///     .route("/", get(handle_dashboard))
    ///     .route("/status", get(handle_status));
    ///
    /// let endpoint = Endpoint::from_router("/diagnostics".to_string(), router);
    /// // This will handle:
    /// // - /diagnostics/
    /// // - /diagnostics/status
    /// ```
    pub(crate) fn from_router(path: String, router: axum::Router) -> Self {
        Self {
            path,
            handler: EndpointHandler::Router(router),
        }
    }

    pub(crate) fn into_router(self) -> axum::Router {
        match self.handler {
            // If we already have a router, just nest it at the path
            EndpointHandler::Router(router) => axum::Router::new().nest(&self.path, router),
            // Legacy service handling with path-based routing
            EndpointHandler::Service(handler) => {
                let handler_clone = handler.clone();
                let handler = move |req: http::Request<axum::body::Body>| {
                    let endpoint = handler_clone.clone();
                    async move {
                        Ok(endpoint
                            .oneshot(req.into())
                            .await
                            .map(|res| res.response)
                            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
                            .into_response())
                    }
                };

                axum::Router::new().route_service(self.path.as_str(), service_fn(handler))
            }
        }
    }
}
/// Factory for creating a router service instance.
///
/// The HTTP server calls `create` once per reload and shares the resulting
/// service across every connection it serves.
pub(crate) trait RouterFactory: Clone + Send + 'static {
    fn create(&self) -> router::BoxCloneService;

    fn web_endpoints(&self) -> MultiMap<ListenAddr, Endpoint>;

    /// Returns the handle for this factory's pipeline. Hold a clone for as long as
    /// requests are still served from this pipeline, including across a reload that
    /// replaces this factory.
    fn pipeline_handle(&self) -> Arc<PipelineHandle>;
}

/// Factory for creating a RouterFactory
///
/// Instances of this traits are used by the StateMachine to generate a new
/// RouterFactory from configuration when it changes
#[async_trait::async_trait]
pub(crate) trait RouterServiceFactory {
    type RouterFactory: RouterFactory;

    async fn create_pipeline(
        &mut self,
        is_telemetry_disabled: bool,
        configuration: Arc<Configuration>,
        schema: Arc<Schema>,
        previous_router: Option<Self::RouterFactory>,
        extra_plugins: Option<Vec<(String, Box<dyn DynPlugin>)>>,
        license: Arc<LicenseState>,
    ) -> Result<Self::RouterFactory, BoxError>;
}

/// Main implementation of the SupergraphService factory, supporting the extensions system
#[derive(Default)]
pub(crate) struct YamlRouterFactory;

#[async_trait::async_trait]
impl RouterServiceFactory for YamlRouterFactory {
    type RouterFactory = RouterCreator;

    async fn create_pipeline(
        &mut self,
        _is_telemetry_disabled: bool,
        configuration: Arc<Configuration>,
        schema: Arc<Schema>,
        previous_router: Option<Self::RouterFactory>,
        extra_plugins: Option<Vec<(String, Box<dyn DynPlugin>)>>,
        license: Arc<LicenseState>,
    ) -> Result<Self::RouterFactory, BoxError> {
        let previous_config: Option<Arc<Configuration>> =
            previous_router.as_ref().map(|r| r.configuration.clone());
        let previous_cache = previous_router.as_ref().map(|r| r.previous_cache());

        crate::pipeline::build_pipeline(
            configuration,
            schema,
            previous_config,
            previous_cache,
            extra_plugins,
            license,
        )
        .instrument(tracing::info_span!(STARTING_SPAN_NAME))
        .await
    }
}

impl TlsClient {
    pub(crate) fn create_certificate_store(
        &self,
    ) -> Option<Result<RootCertStore, ConfigurationError>> {
        self.certificate_authorities
            .as_deref()
            .map(create_certificate_store)
    }
}

pub(crate) fn create_certificate_store(
    certificate_authorities: &str,
) -> Result<RootCertStore, ConfigurationError> {
    let mut store = RootCertStore::empty();
    let certificates = load_certs(certificate_authorities).map_err(|e| {
        ConfigurationError::CertificateAuthorities {
            error: format!("could not parse the certificate list: {e}"),
        }
    })?;
    for certificate in certificates {
        store
            .add(certificate)
            .map_err(|e| ConfigurationError::CertificateAuthorities {
                error: format!("could not add certificate to root store: {e}"),
            })?;
    }
    if store.is_empty() {
        Err(ConfigurationError::CertificateAuthorities {
            error: "the certificate list is empty".to_string(),
        })
    } else {
        Ok(store)
    }
}

fn load_certs(certificates: &str) -> io::Result<Vec<CertificateDer<'static>>> {
    tracing::debug!("loading root certificates");

    // Load and return certificate.
    rustls_pemfile::certs(&mut certificates.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        // XXX(@goto-bus-stop): the error type here is already io::Error. Should we wrap it,
        // instead of replacing it with this generic error message?
        .map_err(|_| io::Error::other("failed to load certificate"))
}

/// test only helper method to create a router factory in integration tests
///
/// not meant to be used directly
pub async fn create_test_service_factory_from_yaml(schema: &str, configuration: &str) {
    let config: Configuration = serde_yaml::from_str(configuration).unwrap();
    let schema = Arc::new(Schema::parse(schema, &config).unwrap());

    let is_telemetry_disabled = false;
    let service = YamlRouterFactory
        .create_pipeline(
            is_telemetry_disabled,
            Arc::new(config),
            schema,
            None,
            None,
            Default::default(),
        )
        .await;
    assert_eq!(
        service.map(|_| ()).unwrap_err().to_string().as_str(),
        r#"failed to initialize the query planner: An internal error has occurred, please report this bug to Apollo.

Details: Object field "Product.reviews"'s inner type "Review" does not refer to an existing output type."#
    );
}

#[cfg(test)]
mod test {
    use std::collections::HashSet;
    use std::sync::Arc;

    use rstest::rstest;
    use schemars::JsonSchema;
    use serde::Deserialize;
    use serde_json::json;
    use tower_http::BoxError;

    use crate::AllowedFeature;
    use crate::configuration::Configuration;
    use crate::pipeline::inject_schema_id;
    use crate::plugin::Plugin;
    use crate::plugin::PluginInit;
    use crate::router_factory::RouterServiceFactory;
    use crate::router_factory::YamlRouterFactory;
    use crate::spec::Schema;
    use crate::uplink::license_enforcement::LicenseLimits;
    use crate::uplink::license_enforcement::LicenseState;

    const MANDATORY_PLUGINS: &[&str] = &[
        "apollo.include_subgraph_errors",
        "apollo.headers",
        "apollo.license_enforcement",
        "apollo.health_check",
        "apollo.traffic_shaping",
        "apollo.limits",
        "apollo.csrf",
        "apollo.fleet_detector",
        "apollo.enhanced_client_awareness",
        "apollo.progressive_override",
    ];

    const OSS_PLUGINS: &[&str] = &[
        "apollo.forbid_mutations",
        "apollo.override_subgraph_url",
        "apollo.connectors",
    ];

    // Always starts and stops plugin

    #[derive(Debug)]
    struct AlwaysStartsAndStopsPlugin {}

    /// Configuration for the test plugin
    #[derive(Debug, Default, Deserialize, JsonSchema)]
    struct Conf {
        /// The name of the test
        name: String,
    }

    #[async_trait::async_trait]
    impl Plugin for AlwaysStartsAndStopsPlugin {
        type Config = Conf;

        async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
            tracing::debug!("{}", init.config.name);
            Ok(AlwaysStartsAndStopsPlugin {})
        }
    }

    register_plugin!(
        "test",
        "always_starts_and_stops",
        AlwaysStartsAndStopsPlugin
    );

    // Always fails to start plugin

    #[derive(Debug)]
    struct AlwaysFailsToStartPlugin {}

    #[async_trait::async_trait]
    impl Plugin for AlwaysFailsToStartPlugin {
        type Config = Conf;

        async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
            tracing::debug!("{}", init.config.name);
            Err(BoxError::from("Error"))
        }
    }

    register_plugin!("test", "always_fails_to_start", AlwaysFailsToStartPlugin);

    async fn create_service(config: Configuration) -> Result<(), BoxError> {
        let schema = include_str!("testdata/supergraph.graphql");
        let schema = Schema::parse(schema, &config)?;

        let is_telemetry_disabled = false;
        let service = YamlRouterFactory
            .create_pipeline(
                is_telemetry_disabled,
                Arc::new(config),
                Arc::new(schema),
                None,
                None,
                Arc::new(LicenseState::default()),
            )
            .await;
        service.map(|_| ())
    }

    #[tokio::test]
    async fn test_yaml_no_extras() {
        let config = Configuration::builder().build().unwrap();
        let service = create_service(config).await;
        assert!(service.is_ok())
    }

    #[tokio::test]
    async fn test_yaml_plugins_always_starts_and_stops() {
        let config: Configuration = serde_yaml::from_str(
            r#"
            plugins:
                test.always_starts_and_stops:
                    name: albert
        "#,
        )
        .unwrap();
        let service = create_service(config).await;
        assert!(service.is_ok())
    }

    #[tokio::test]
    async fn test_yaml_plugins_always_fails_to_start() {
        let config: Configuration = serde_yaml::from_str(
            r#"
            plugins:
                test.always_fails_to_start:
                    name: albert
        "#,
        )
        .unwrap();
        let service = create_service(config).await;
        assert!(service.is_err())
    }

    #[tokio::test]
    async fn test_yaml_plugins_combo_start_and_fail() {
        let config: Configuration = serde_yaml::from_str(
            r#"
            plugins:
                test.always_starts_and_stops:
                    name: albert
                test.always_fails_to_start:
                    name: albert
        "#,
        )
        .unwrap();
        let service = create_service(config).await;
        assert!(service.is_err())
    }

    #[test]
    fn test_inject_schema_id() {
        let mut config = json!({ "apollo": {} });
        inject_schema_id(
            "8e2021d131b23684671c3b85f82dfca836908c6a541bbd5c3772c66e7f8429d8",
            &mut config,
        );
        let config =
            serde_json::from_value::<crate::plugins::telemetry::config::Conf>(config).unwrap();
        assert_eq!(
            &config.apollo.schema_id,
            "8e2021d131b23684671c3b85f82dfca836908c6a541bbd5c3772c66e7f8429d8"
        );
    }

    fn get_plugin_config(plugin: &str) -> &str {
        match plugin {
            "subscription" => {
                r#"
                enabled: true
                "#
            }
            "authentication" => {
                r#"
                connector:
                  sources: {}
                "#
            }
            "authorization" => {
                r#"
                require_authentication: false
                "#
            }
            "preview_file_uploads" => {
                r#"
                enabled: true
                protocols:
                  multipart:
                    enabled: false
                "#
            }
            "response_cache" => {
                r#"
                enabled: true
                subgraph:
                  all:
                    enabled: true
                "#
            }
            "demand_control" => {
                r#"
                enabled: true
                mode: measure
                strategy:
                  static_estimated:
                    list_size: 0
                    max: 0.0
                "#
            }
            "coprocessor" => {
                r#"
                url: http://service.example.com/url
                "#
            }
            "connectors" => {
                r#"
                debug_extensions: false
                "#
            }
            "experimental_mock_subgraphs" => {
                r#"
               subgraphs: {}
                "#
            }
            "forbid_mutations" => {
                r#"
                false
                "#
            }
            "override_subgraph_url" => {
                r#"
                {}
                "#
            }
            _ => panic!("This function does not contain config for plugin: {plugin}"),
        }
    }

    #[tokio::test]
    #[rstest]
    #[case::empty_allowed_features_set(HashSet::new())]
    #[case::nonempty_allowed_features_set(HashSet::from_iter(vec![AllowedFeature::Coprocessors]))]
    async fn test_mandatory_plugins_added(#[case] allowed_features: HashSet<AllowedFeature>) {
        /*
         * GIVEN
         *  - a valid license
         *  - a valid config
         *  - a valid schema
         * */
        let license = LicenseState::Licensed {
            limits: Some(LicenseLimits {
                tps: None,
                allowed_features,
            }),
        };

        let router_config = Configuration::builder().build().unwrap();
        let schema = include_str!("testdata/supergraph.graphql");
        let schema = Schema::parse(schema, &router_config).unwrap();

        /*
         * WHEN
         *  - the router factory runs (including the plugin inits gated by the license)
         * */
        let is_telemetry_disabled = false;
        let service = YamlRouterFactory
            .create_pipeline(
                is_telemetry_disabled,
                Arc::new(router_config),
                Arc::new(schema),
                None,
                None,
                Arc::new(license),
            )
            .await
            .unwrap();

        /*
         * THEN
         *  - the mandatory plugins are added
         * */
        assert!(
            MANDATORY_PLUGINS
                .iter()
                .all(|plugin| { service.plugins.contains_key(*plugin) })
        );
    }

    #[tokio::test]
    #[rstest]
    #[case::allowed_features_empty(HashSet::new())]
    #[case::allowed_features_nonempty(HashSet::from_iter(vec![
        AllowedFeature::Coprocessors,
        AllowedFeature::DemandControl
    ]))]
    async fn test_oss_plugins_added(#[case] allowed_features: HashSet<AllowedFeature>) {
        /*
         * GIVEN
         *  - a valid license
         *  - a valid config that contains configuration for oss plugins
         *  - a valid schema
         * */
        let license = LicenseState::Licensed {
            limits: Some(LicenseLimits {
                tps: None,
                allowed_features,
            }),
        };

        // Create config for oss plugins
        let forbid_mutations_config =
            serde_yaml::from_str::<serde_json::Value>(get_plugin_config("forbid_mutations"))
                .unwrap();
        let override_subgraph_url_config =
            serde_yaml::from_str::<serde_json::Value>(get_plugin_config("override_subgraph_url"))
                .unwrap();
        let connectors_config =
            serde_yaml::from_str::<serde_json::Value>(get_plugin_config("connectors")).unwrap();

        let router_config = Configuration::builder()
            .apollo_plugin("forbid_mutations", forbid_mutations_config)
            .apollo_plugin("override_subgraph_url", override_subgraph_url_config)
            .apollo_plugin("connectors", connectors_config)
            .build()
            .unwrap();

        let schema = include_str!("testdata/supergraph.graphql");
        let schema = Schema::parse(schema, &router_config).unwrap();

        /*
         * WHEN
         *  - the router factory runs (including the plugin inits gated by the license)
         * */
        let is_telemetry_disabled = false;
        let service = YamlRouterFactory
            .create_pipeline(
                is_telemetry_disabled,
                Arc::new(router_config),
                Arc::new(schema),
                None,
                None,
                Arc::new(license),
            )
            .await
            .unwrap();

        /*
         * THEN
         *  - all oss plugins should have been added
         * */
        assert!(
            OSS_PLUGINS
                .iter()
                .all(|plugin| { service.plugins.contains_key(*plugin) })
        );
    }

    #[tokio::test]
    #[rstest]
    #[case::subscripions(
        "subscription",
        HashSet::from_iter(vec![AllowedFeature::DemandControl, AllowedFeature::Subscriptions]))
    ]
    #[case::authorization(
        "authorization",
        HashSet::from_iter(vec![AllowedFeature::Authorization, AllowedFeature::Subscriptions]))
    ]
    #[case::authentication(
        "authentication",
        HashSet::from_iter(vec![AllowedFeature::DemandControl, AllowedFeature::Authentication, AllowedFeature::Subscriptions]))
    ]
    #[case::response_cache(
        "response_cache",
        HashSet::from_iter(vec![AllowedFeature::DemandControl, AllowedFeature::ResponseCaching]))
    ]
    #[case::authorization(
        "demand_control",
        HashSet::from_iter(vec![AllowedFeature::Authorization, AllowedFeature::Subscriptions, AllowedFeature::DemandControl]))
    ]
    #[case::coprocessor(
        "coprocessor",
        HashSet::from_iter(vec![AllowedFeature::Coprocessors, AllowedFeature::DemandControl]))
    ]
    async fn test_optional_plugin_added_with_restricted_allowed_features(
        #[case] plugin: &str,
        #[case] allowed_features: HashSet<AllowedFeature>,
    ) {
        /*
         * GIVEN
         *  - a restricted license with allowed feature set containing the given `plugin`
         *  - a valid config including valid config for the given `plugin`
         *  - a valid schema
         * */
        let license = LicenseState::Licensed {
            limits: Some(LicenseLimits {
                tps: None,
                allowed_features,
            }),
        };

        let plugin_config =
            serde_yaml::from_str::<serde_json::Value>(get_plugin_config(plugin)).unwrap();
        dbg!(&plugin_config);
        let router_config = Configuration::builder()
            .apollo_plugin(plugin, plugin_config)
            .build()
            .unwrap();

        let schema = include_str!("testdata/supergraph.graphql");
        let schema = Schema::parse(schema, &router_config).unwrap();

        /*
         * WHEN
         *  - the router factory runs (including the plugin inits gated by the license)
         * */
        let is_telemetry_disabled = false;
        let service = YamlRouterFactory
            .create_pipeline(
                is_telemetry_disabled,
                Arc::new(router_config),
                Arc::new(schema),
                None,
                None,
                Arc::new(license),
            )
            .await
            .unwrap();

        /*
         * THEN
         *  - since the plugin is part of the `allowed_features` set
         *    the plugin should have been added.
         * - mandatory plugins should have been added.
         * */
        assert!(
            service.plugins.contains_key(&format!("apollo.{plugin}")),
            "Plugin {plugin} should have been added"
        );
        assert!(
            MANDATORY_PLUGINS
                .iter()
                .all(|plugin| { service.plugins.contains_key(*plugin) })
        );
    }

    #[tokio::test]
    #[rstest]
    #[case::subscripions(
        "subscription",
        HashSet::from_iter(vec![]))
    ]
    #[case::authorization(
        "authorization",
        HashSet::from_iter(vec![AllowedFeature::Authentication, AllowedFeature::Subscriptions]))
    ]
    #[case::authentication(
        "authentication",
        HashSet::from_iter(vec![AllowedFeature::DemandControl,AllowedFeature::Subscriptions]))
    ]
    #[case::response_cache(
        "response_cache",
        HashSet::from_iter(vec![AllowedFeature::Authentication]))
    ]
    #[case::authorization(
        "demand_control",
        HashSet::from_iter(vec![AllowedFeature::Authorization, AllowedFeature::Subscriptions, AllowedFeature::Experimental]))
    ]
    #[case::coprocessor(
        "coprocessor",
        HashSet::from_iter(vec![AllowedFeature::DemandControl]))
    ]
    async fn test_optional_plugin_not_added_with_restricted_allowed_features(
        #[case] plugin: &str,
        #[case] allowed_features: HashSet<AllowedFeature>,
    ) {
        /*
         * GIVEN
         *  - a restricted license whose allowed feature set does not contain the given `plugin`
         *  - a valid config including valid config for the given `plugin`
         *  - a valid schema
         * */
        let license = LicenseState::Licensed {
            limits: Some(LicenseLimits {
                tps: None,
                allowed_features,
            }),
        };

        let plugin_config =
            serde_yaml::from_str::<serde_json::Value>(get_plugin_config(plugin)).unwrap();
        let router_config = Configuration::builder()
            .apollo_plugin(plugin, plugin_config)
            .build()
            .unwrap();

        let schema = include_str!("testdata/supergraph.graphql");
        let schema = Schema::parse(schema, &router_config).unwrap();

        /*
         * WHEN
         *  - the router factory runs (including the plugin inits gated by the license)
         * */
        let is_telemetry_disabled = false;
        let service = YamlRouterFactory
            .create_pipeline(
                is_telemetry_disabled,
                Arc::new(router_config),
                Arc::new(schema),
                None,
                None,
                Arc::new(license),
            )
            .await
            .unwrap();

        /*
         * THEN
         *  - since the plugin is not part of the `allowed_features` set
         *    the plugin should not have been added.
         * - mandatory plugins should have been added.
         * */
        assert!(
            !service.plugins.contains_key(&format!("apollo.{plugin}")),
            "Plugin {plugin} should not have been added"
        );
        assert!(
            MANDATORY_PLUGINS
                .iter()
                .all(|plugin| { service.plugins.contains_key(*plugin) })
        );
    }

    #[tokio::test]
    #[rstest]
    #[case::mock_subgraphs_non_empty_allowed_features(
        "experimental_mock_subgraphs",
        HashSet::from_iter(vec![AllowedFeature::DemandControl])
    )]
    #[case::mock_subgraphs_empty_allowed_features(
        "experimental_mock_subgraphs",
        HashSet::from_iter(vec![])
    )]
    async fn test_optional_plugin_that_does_not_map_to_an_allowed_feature_is_added(
        #[case] plugin: &str,
        #[case] allowed_features: HashSet<AllowedFeature>,
    ) {
        /*
         * GIVEN
         *  - a valid license
         *  - a valid config including valid config for the optional plugin that does
         *    not map to an allowed feature
         *  - a valid schema
         * */
        let license = LicenseState::Licensed {
            limits: Some(LicenseLimits {
                tps: None,
                allowed_features,
            }),
        };

        let plugin_config =
            serde_yaml::from_str::<serde_json::Value>(get_plugin_config(plugin)).unwrap();
        let router_config = Configuration::builder()
            .apollo_plugin(plugin, plugin_config)
            .build()
            .unwrap();

        let schema = include_str!("testdata/supergraph.graphql");
        let schema = Schema::parse(schema, &router_config).unwrap();

        /*
         * WHEN
         *  - the router factory runs (including the plugin inits gated by the license)
         * */
        let is_telemetry_disabled = false;
        let service = YamlRouterFactory
            .create_pipeline(
                is_telemetry_disabled,
                Arc::new(router_config),
                Arc::new(schema),
                None,
                None,
                Arc::new(license),
            )
            .await
            .unwrap();

        /*
         * THEN
         * - the plugin should be added
         * - mandatory plugins should have been added.
         * - coprocessors and subscritions (both gated features) should not have been added.
         * */
        assert!(
            service.plugins.contains_key(&format!("apollo.{plugin}")),
            "Plugin {plugin} should have been added"
        );
        assert!(
            MANDATORY_PLUGINS
                .iter()
                .all(|plugin| { service.plugins.contains_key(*plugin) })
        );
        // These gated features should not have been added
        assert!(
            !service.plugins.contains_key("apollo.subscription"),
            "Plugin {plugin} should not have been added"
        );
        assert!(
            !service.plugins.contains_key("apollo.coprocessor"),
            "Plugin {plugin} should not have been added"
        );
    }

    #[tokio::test]
    #[rstest]
    // NB: this is temporary behavior and will change once the `allowed_features` claim is in all licenses
    #[case::forbid_mutations("forbid_mutations")]
    #[case::subscriptions("subscription")]
    #[case::override_subgraph_url("override_subgraph_url")]
    #[case::authorization("authorization")]
    #[case::authentication("authentication")]
    #[case::file_upload("preview_file_uploads")]
    #[case::response_cache("response_cache")]
    #[case::demand_control("demand_control")]
    #[case::connectors("connectors")]
    #[case::coprocessor("coprocessor")]
    #[case::mock_subgraphs("experimental_mock_subgraphs")]
    async fn test_optional_plugin_with_unrestricted_allowed_features(#[case] plugin: &str) {
        /*
         * GIVEN
         *  - a license with unrestricted limits (includes allowing all features)
         *  - a valid config including valid config for the given `plugin`
         *  - a valid schema
         * */
        let license = LicenseState::Licensed {
            limits: Default::default(),
        };

        let plugin_config =
            serde_yaml::from_str::<serde_json::Value>(get_plugin_config(plugin)).unwrap();
        let router_config = Configuration::builder()
            .apollo_plugin(plugin, plugin_config)
            .build()
            .unwrap();

        let schema = include_str!("testdata/supergraph.graphql");
        let schema = Schema::parse(schema, &router_config).unwrap();

        /*
         * WHEN
         *  - the router factory runs (including the plugin inits gated by the license)
         * */
        let is_telemetry_disabled = false;
        let service = YamlRouterFactory
            .create_pipeline(
                is_telemetry_disabled,
                Arc::new(router_config),
                Arc::new(schema),
                None,
                None,
                Arc::new(license),
            )
            .await
            .unwrap();

        /*
         * THEN
         *  - since `allowed_features` is unrestricted plugin should have been added.
         * */
        assert!(
            service.plugins.contains_key(&format!("apollo.{plugin}")),
            "Plugin {plugin} should have been added"
        );
        assert!(
            MANDATORY_PLUGINS
                .iter()
                .all(|plugin| { service.plugins.contains_key(*plugin) })
        );
    }

    #[tokio::test]
    #[rstest]
    // NB: this is temporary behavior and will change once the `allowed_features` claim is in all licenses
    #[case::forbid_mutations("forbid_mutations")]
    #[case::subscriptions("subscription")]
    #[case::override_subgraph_url("override_subgraph_url")]
    #[case::authorization("authorization")]
    #[case::authentication("authentication")]
    #[case::file_upload("preview_file_uploads")]
    #[case::response_cache("response_cache")]
    #[case::demand_control("demand_control")]
    #[case::connectors("connectors")]
    #[case::coprocessor("coprocessor")]
    #[case::mock_subgraphs("experimental_mock_subgraphs")]
    async fn test_optional_plugin_with_default_license_limits(#[case] plugin: &str) {
        /*
         * GIVEN
         *  - a license with license limits None
         *  - a valid config including valid config for the given `plugin`
         *  - a valid schema
         * */
        let license = LicenseState::Licensed {
            limits: Default::default(),
        };

        // Create config for the given `plugin`
        let plugin_config =
            serde_yaml::from_str::<serde_json::Value>(get_plugin_config(plugin)).unwrap();

        // Create config for oss plugins
        // Create config for oss plugins
        let forbid_mutations_config =
            serde_yaml::from_str::<serde_json::Value>(get_plugin_config("forbid_mutations"))
                .unwrap();
        let override_subgraph_url_config =
            serde_yaml::from_str::<serde_json::Value>(get_plugin_config("override_subgraph_url"))
                .unwrap();
        let connectors_config =
            serde_yaml::from_str::<serde_json::Value>(get_plugin_config("connectors")).unwrap();
        let response_cache_config =
            serde_yaml::from_str::<serde_json::Value>(get_plugin_config("response_cache")).unwrap();

        let router_config = Configuration::builder()
            .apollo_plugin("forbid_mutations", forbid_mutations_config)
            .apollo_plugin("override_subgraph_url", override_subgraph_url_config)
            .apollo_plugin("connectors", connectors_config)
            .apollo_plugin("response_cache", response_cache_config)
            .apollo_plugin(plugin, plugin_config)
            .build()
            .unwrap();

        let schema = include_str!("testdata/supergraph.graphql");
        let schema = Schema::parse(schema, &router_config).unwrap();

        /*
         * WHEN
         *  - the router factory runs (including the plugin inits gated by the license)
         * */
        let is_telemetry_disabled = false;
        let service = YamlRouterFactory
            .create_pipeline(
                is_telemetry_disabled,
                Arc::new(router_config),
                Arc::new(schema),
                None,
                None,
                Arc::new(license),
            )
            .await
            .unwrap();

        /*
         * THEN
         *  // NB: this behavior may change once all licenses have an `allowed_features` claim
         *  - when license limits are None we default to unrestricted allowed features
         *  - the given `plugin` should have been added
         *  - all mandatory plugins should have been added
         *  - all oss plugins in the config should have been added
         * */
        assert!(
            service.plugins.contains_key(&format!("apollo.{plugin}")),
            "Plugin {plugin} should have been added"
        );
        assert!(
            MANDATORY_PLUGINS
                .iter()
                .all(|plugin| { service.plugins.contains_key(*plugin) })
        );
        assert!(
            OSS_PLUGINS
                .iter()
                .all(|plugin| { service.plugins.contains_key(*plugin) })
        );
    }
}

#[cfg(test)]
mod create_subgraph_services_tests {
    use std::convert::Infallible;
    use std::str::FromStr;
    use std::sync::Arc;

    use axum::body::Body;
    use bytes::Buf;
    use http::StatusCode;
    use http::Uri;
    use http::header::CONTENT_TYPE;
    use indexmap::IndexMap;
    use mime::APPLICATION_JSON;
    use serde_json_bytes::ByteString;
    use tokio::net::TcpListener;
    use tower::ServiceExt;

    use crate::Configuration;
    use crate::Context;
    use crate::configuration::SubgraphApq;
    use crate::graphql::Response;
    use crate::pipeline::create_subgraph_services;
    use crate::query_planner::fetch::OperationKind;
    use crate::services::SubgraphRequest;
    use crate::services::http::HttpClientServiceFactory;
    use crate::services::layers::apq::subgraph::PERSISTED_QUERY_KEY;
    use crate::services::router;
    use crate::services::subgraph::service::SubgraphServiceFactory;

    async fn serve<Handler, Fut>(listener: TcpListener, handle: Handler) -> std::io::Result<()>
    where
        Handler: (Fn(http::Request<Body>) -> Fut) + Clone + Sync + Send + 'static,
        Fut:
            std::future::Future<Output = Result<http::Response<Body>, Infallible>> + Send + 'static,
    {
        use hyper::body::Incoming;
        use hyper_util::rt::TokioExecutor;
        use hyper_util::rt::TokioIo;

        loop {
            let (stream, _) = listener.accept().await?;
            let io = TokioIo::new(stream);
            let handle = handle.clone();
            tokio::spawn(async move {
                let svc = hyper::service::service_fn(|request: http::Request<Incoming>| {
                    handle(request.map(Body::new))
                });
                if let Err(err) = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new())
                    .serve_connection_with_upgrades(io, svc)
                    .await
                {
                    eprintln!("server error: {err}");
                }
            });
        }
    }

    async fn parse_graphql_request(body: Body) -> crate::graphql::Request {
        let bytes = router::body::into_bytes(body)
            .await
            .expect("can read request body");
        serde_json::from_reader(bytes.reader()).expect("valid graphql request")
    }

    fn subgraph_request(uri: Uri, subgraph_name: &str, query: &str) -> SubgraphRequest {
        SubgraphRequest::builder()
            .supergraph_request(Arc::new(
                http::Request::builder()
                    .body(crate::graphql::Request::builder().query(query).build())
                    .unwrap(),
            ))
            .subgraph_request(
                http::Request::builder()
                    .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                    .uri(uri)
                    .body(crate::graphql::Request::builder().query(query).build())
                    .unwrap(),
            )
            .operation_kind(OperationKind::Query)
            .subgraph_name(subgraph_name.to_string())
            .context(Context::new())
            .build()
    }

    fn make_http_service_factory(name: &str) -> HttpClientServiceFactory {
        HttpClientServiceFactory::from_config(
            name,
            &Configuration::default(),
            crate::configuration::shared::Client::default(),
        )
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn respects_all_enabled_config() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socket_addr = listener.local_addr().unwrap();
        tokio::task::spawn(async move {
            serve(listener, |request| async move {
                let graphql_request = parse_graphql_request(request.into_body()).await;
                assert!(graphql_request.extensions.contains_key(PERSISTED_QUERY_KEY));
                assert!(graphql_request.query.is_none());
                Ok(http::Response::builder()
                    .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                    .status(StatusCode::OK)
                    .body(
                        serde_json::to_string(&Response {
                            data: Some(serde_json_bytes::Value::String(ByteString::from("test"))),
                            ..Response::default()
                        })
                        .unwrap()
                        .into(),
                    )
                    .unwrap())
            })
            .await
            .unwrap();
        });

        let mut config = Configuration::default();
        config.apq.subgraph.all.enabled = true;

        let mut http_service_factory = IndexMap::new();
        http_service_factory.insert("test".to_string(), make_http_service_factory("test"));

        let subgraph_services = create_subgraph_services(&http_service_factory);
        let factory = SubgraphServiceFactory::new(
            subgraph_services.into_iter().collect(),
            Default::default(),
            Default::default(),
            None,
            config.apq.subgraph.clone(),
        );
        let url = Uri::from_str(&format!("http://{socket_addr}")).unwrap();
        let resp = factory
            .get("test")
            .unwrap()
            .oneshot(subgraph_request(url, "test", "query"))
            .await
            .unwrap();

        assert_eq!(
            resp.response.body().data,
            Some(serde_json_bytes::Value::String(ByteString::from("test")))
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn respects_all_disabled_config() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socket_addr = listener.local_addr().unwrap();
        tokio::task::spawn(async move {
            serve(listener, |request| async move {
                let graphql_request = parse_graphql_request(request.into_body()).await;
                assert!(!graphql_request.extensions.contains_key(PERSISTED_QUERY_KEY));
                assert!(graphql_request.query.is_some());
                Ok(http::Response::builder()
                    .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                    .status(StatusCode::OK)
                    .body(
                        serde_json::to_string(&Response {
                            data: Some(serde_json_bytes::Value::String(ByteString::from("test"))),
                            ..Response::default()
                        })
                        .unwrap()
                        .into(),
                    )
                    .unwrap())
            })
            .await
            .unwrap();
        });

        let mut config = Configuration::default();
        config.apq.subgraph.all.enabled = false;

        let mut http_service_factory = IndexMap::new();
        http_service_factory.insert("test".to_string(), make_http_service_factory("test"));

        let subgraph_services = create_subgraph_services(&http_service_factory);
        let factory = SubgraphServiceFactory::new(
            subgraph_services.into_iter().collect(),
            Default::default(),
            Default::default(),
            None,
            config.apq.subgraph.clone(),
        );
        let url = Uri::from_str(&format!("http://{socket_addr}")).unwrap();
        factory
            .get("test")
            .unwrap()
            .oneshot(subgraph_request(url, "test", "query"))
            .await
            .unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn per_subgraph_override_takes_precedence_over_all() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socket_addr = listener.local_addr().unwrap();
        tokio::task::spawn(async move {
            serve(listener, |request| async move {
                let subgraph_name = request
                    .headers()
                    .get("x-subgraph-name")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or_default()
                    .to_string();
                let graphql_request = parse_graphql_request(request.into_body()).await;

                match subgraph_name.as_str() {
                    "enabled_subgraph" => {
                        assert!(graphql_request.extensions.contains_key(PERSISTED_QUERY_KEY));
                        assert!(graphql_request.query.is_none());
                    }
                    "disabled_subgraph" => {
                        assert!(!graphql_request.extensions.contains_key(PERSISTED_QUERY_KEY));
                        assert!(graphql_request.query.is_some());
                    }
                    other => panic!("unexpected subgraph name: {other}"),
                }

                Ok(http::Response::builder()
                    .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                    .status(StatusCode::OK)
                    .body(
                        serde_json::to_string(&Response {
                            data: Some(serde_json_bytes::Value::String(ByteString::from("test"))),
                            ..Response::default()
                        })
                        .unwrap()
                        .into(),
                    )
                    .unwrap())
            })
            .await
            .unwrap();
        });

        let mut config = Configuration::default();
        config.apq.subgraph.all.enabled = false;
        config.apq.subgraph.subgraphs.insert(
            "enabled_subgraph".to_string(),
            SubgraphApq { enabled: true },
        );

        let mut http_service_factory = IndexMap::new();
        http_service_factory.insert(
            "enabled_subgraph".to_string(),
            make_http_service_factory("enabled_subgraph"),
        );
        http_service_factory.insert(
            "disabled_subgraph".to_string(),
            make_http_service_factory("disabled_subgraph"),
        );

        let subgraph_services = create_subgraph_services(&http_service_factory);
        let factory = SubgraphServiceFactory::new(
            subgraph_services.into_iter().collect(),
            Default::default(),
            Default::default(),
            None,
            config.apq.subgraph.clone(),
        );
        let url = Uri::from_str(&format!("http://{socket_addr}")).unwrap();

        let enabled_request = SubgraphRequest::builder()
            .supergraph_request(Arc::new(
                http::Request::builder()
                    .body(crate::graphql::Request::builder().query("query").build())
                    .unwrap(),
            ))
            .subgraph_request(
                http::Request::builder()
                    .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                    .header("x-subgraph-name", "enabled_subgraph")
                    .uri(url.clone())
                    .body(crate::graphql::Request::builder().query("query").build())
                    .unwrap(),
            )
            .operation_kind(OperationKind::Query)
            .subgraph_name("enabled_subgraph".to_string())
            .context(Context::new())
            .build();

        let disabled_request = SubgraphRequest::builder()
            .supergraph_request(Arc::new(
                http::Request::builder()
                    .body(crate::graphql::Request::builder().query("query").build())
                    .unwrap(),
            ))
            .subgraph_request(
                http::Request::builder()
                    .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                    .header("x-subgraph-name", "disabled_subgraph")
                    .uri(url)
                    .body(crate::graphql::Request::builder().query("query").build())
                    .unwrap(),
            )
            .operation_kind(OperationKind::Query)
            .subgraph_name("disabled_subgraph".to_string())
            .context(Context::new())
            .build();

        factory
            .get("enabled_subgraph")
            .unwrap()
            .oneshot(enabled_request)
            .await
            .unwrap();
        factory
            .get("disabled_subgraph")
            .unwrap()
            .oneshot(disabled_request)
            .await
            .unwrap();
    }
}
