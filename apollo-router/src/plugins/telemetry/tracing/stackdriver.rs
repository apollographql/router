//! Configuration for Stackdriver tracing.

use futures::executor;
use opentelemetry::sdk;
use opentelemetry::sdk::trace::BatchSpanProcessor;
use opentelemetry::sdk::trace::Builder;

use opentelemetry_stackdriver::GcpAuthorizer;
use opentelemetry_stackdriver::StackDriverExporter;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use tower::BoxError;

use crate::plugins::telemetry::config::Trace;
use crate::plugins::telemetry::tracing::BatchProcessorConfig;
use crate::plugins::telemetry::tracing::TracingConfigurator;

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct Config {
    /// batch processor configuration
    #[serde(default)]
    pub(crate) batch_processor: BatchProcessorConfig,
}

impl TracingConfigurator for Config {
    fn apply(&self, builder: Builder, trace: &Trace) -> Result<Builder, BoxError> {
        tracing::info!("configuring Stackdriver tracing: {}", self.batch_processor);
        let trace_config: sdk::trace::Config = trace.into();
        tracing::info!("Stackdriver trace_config: {:#?}", trace_config);

        // let res = init_stackdriver(&self, builder);
        let res_future = async {
            tracing::info!("Stackdriver authenticator before");

            let authenticator_instance = GcpAuthorizer::new().await;

            let authenticator = match authenticator_instance {
                Ok(authenticator) => authenticator,
                Err(error) => panic!("Stackdriver authenticator error: {:?}", error),
            };
            tracing::info!("Stackdriver authenticator after");

            tracing::info!("Stackdriver exporter before");

            let stackdriver_builder_instance =
                StackDriverExporter::builder().build(authenticator).await;

            let (exporter, whatevers_rest) = match stackdriver_builder_instance {
                Ok((exporter, rest)) => (exporter, rest),
                Err(error) => panic!(
                    "Stackdriver stackdriver_builder_instance error: {:?}",
                    error
                ),
            };

            // let (exporter, _) = StackDriverExporter::builder()
            //     .build(authenticator)
            //     .await
            //     .unwrap();

            tracing::info!("Stackdriver driver: {:#?}", whatevers_rest.await);
            tracing::info!("Stackdriver exporter: {:#?}", exporter);

            builder.with_span_processor(
                BatchSpanProcessor::builder(exporter, opentelemetry::runtime::Tokio)
                    .with_batch_config(self.batch_processor.clone().into())
                    .build(),
            )
        };

        tracing::info!("Stackdriver res_future...");

        let res = executor::block_on(res_future);

        tracing::info!("Stackdriver res: {:#?}", res);

        Ok(res)
    }
}
