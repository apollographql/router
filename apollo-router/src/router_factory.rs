use std::sync::Arc;

use multimap::MultiMap;
use tower::BoxError;

use crate::ListenAddr;
use crate::axum_factory::Endpoint;
use crate::configuration::Configuration;
use crate::pipeline::Pipeline;
use crate::plugin::DynPlugin;
use crate::services::router;
use crate::services::router::pipeline_handle::PipelineHandle;
use crate::spec::Schema;
use crate::uplink::license_enforcement::LicenseState;

pub(crate) const STARTING_SPAN_NAME: &str = "starting";

/// A built pipeline's serving surface: the router service, the plugin web endpoints,
/// and the pipeline handle.
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

/// Builds a [`Pipeline`] from configuration via [`crate::pipeline::build_pipeline`].
#[derive(Default)]
pub(crate) struct PipelineFactory;

#[async_trait::async_trait]
impl RouterServiceFactory for PipelineFactory {
    type RouterFactory = Pipeline;

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
        .await
    }
}

/// test only helper method to create a router factory in integration tests
///
/// not meant to be used directly
pub async fn create_test_service_factory_from_yaml(schema: &str, configuration: &str) {
    let config: Configuration = serde_yaml::from_str(configuration).unwrap();
    let schema = Arc::new(Schema::parse(schema, &config).unwrap());

    let is_telemetry_disabled = false;
    let service = PipelineFactory
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
