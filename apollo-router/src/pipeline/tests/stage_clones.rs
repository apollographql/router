//! Cloning a stage's service must not copy the stacks beneath it, because many layers clone the
//! service beneath them on every call.

use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::task::Context;
use std::task::Poll;

use tower::BoxError;
use tower::Service;
use tower::ServiceExt;

use super::test_configuration;
use super::test_schema;
use crate::pipeline::build_query_planner_service;
use crate::pipeline::build_supergraph_pipeline;
use crate::pipeline::create_query_planner;
use crate::pipeline::stages::build_execution_service;
use crate::plugin::PluginInit;
use crate::plugin::PluginUnstable;
use crate::services::Plugins;
use crate::services::SubgraphServices;
use crate::services::execution;
use crate::services::supergraph;

/// Forwards to the service it wraps and counts how many times it is cloned.
struct CountsClones<S> {
    inner: S,
    clones: Arc<AtomicUsize>,
}

impl<S: Clone> Clone for CountsClones<S> {
    fn clone(&self) -> Self {
        self.clones.fetch_add(1, Ordering::Relaxed);
        Self {
            inner: self.inner.clone(),
            clones: Arc::clone(&self.clones),
        }
    }
}

impl<S: Service<Request>, Request> Service<Request> for CountsClones<S> {
    type Response = S::Response;
    type Error = S::Error;
    type Future = S::Future;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, request: Request) -> Self::Future {
        self.inner.call(request)
    }
}

/// Wraps the supergraph and execution plugin hooks in [`CountsClones`].
#[derive(Default)]
struct CountStageClones {
    supergraph: Arc<AtomicUsize>,
    execution: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl PluginUnstable for CountStageClones {
    type Config = ();

    async fn new(_: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        unreachable!("inserted into the plugin registry directly")
    }

    fn supergraph_service(
        &self,
        service: supergraph::BoxCloneService,
    ) -> supergraph::BoxCloneService {
        CountsClones {
            inner: service,
            clones: Arc::clone(&self.supergraph),
        }
        .boxed_clone()
    }

    fn execution_service(&self, service: execution::BoxCloneService) -> execution::BoxCloneService {
        CountsClones {
            inner: service,
            clones: Arc::clone(&self.execution),
        }
        .boxed_clone()
    }

    fn unstable_method(&self) {}
}

/// The stage plugins, with [`CountStageClones`] last, and its supergraph and execution counters.
async fn plugins_counting_stage_clones() -> (Arc<Plugins>, Arc<AtomicUsize>, Arc<AtomicUsize>) {
    let counter = CountStageClones::default();
    let supergraph_clones = Arc::clone(&counter.supergraph);
    let execution_clones = Arc::clone(&counter.execution);
    let mut plugins = Arc::into_inner(super::stage_stack::required_stage_plugins().await)
        .expect("the plugins are not shared yet");
    plugins.insert("count_stage_clones".to_string(), Box::new(counter));
    (Arc::new(plugins), supergraph_clones, execution_clones)
}

#[tokio::test]
async fn cloning_the_supergraph_service_does_not_copy_its_stack() {
    let configuration = test_configuration();
    let schema = test_schema(&configuration);
    let (planner, subgraph_schemas) = create_query_planner(&schema, &configuration).unwrap();
    let query_planner_service = build_query_planner_service(
        schema.clone(),
        configuration.clone(),
        planner,
        subgraph_schemas.clone(),
        crate::pipeline::build_query_plan_cache(&configuration, None),
    );
    let (plugins, supergraph_clones, _) = plugins_counting_stage_clones().await;

    let supergraph_service = build_supergraph_pipeline(
        query_planner_service,
        schema,
        subgraph_schemas,
        configuration,
        plugins,
        SubgraphServices {
            services: Default::default(),
        },
        Default::default(),
    );
    let clones: Vec<_> = (0..10).map(|_| supergraph_service.clone()).collect();

    assert_eq!(clones.len(), 10);
    assert_eq!(supergraph_clones.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn cloning_the_execution_service_does_not_copy_its_stack() {
    let configuration = test_configuration();
    let schema = test_schema(&configuration);
    let (_, subgraph_schemas) = create_query_planner(&schema, &configuration).unwrap();
    let (plugins, _, execution_clones) = plugins_counting_stage_clones().await;

    let execution_service = build_execution_service(
        schema,
        subgraph_schemas,
        plugins,
        SubgraphServices {
            services: Default::default(),
        },
        Default::default(),
        configuration,
    );
    let clones: Vec<_> = (0..10).map(|_| execution_service.clone()).collect();

    assert_eq!(clones.len(), 10);
    assert_eq!(execution_clones.load(Ordering::Relaxed), 0);
}
