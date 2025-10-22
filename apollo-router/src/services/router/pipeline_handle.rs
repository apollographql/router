use std::sync::Arc;

use crate::metrics::UpDownCounterGuard;

/// A pipeline is used to keep track of how many pipelines we have active. It's associated with an instance of RouterCreator
/// The telemetry plugin has a gauge to expose this data
/// Pipeline ref represents a unique pipeline
#[derive(Clone, Hash, Eq, PartialEq, Debug)]
pub(crate) struct PipelineRef {
    pub(crate) schema_id: String,
    pub(crate) launch_id: Option<String>,
    pub(crate) config_hash: String,
}

/// A pipeline handle does the actual tracking of pipelines
/// Creating a new pipeline handle will increment the updown counter.
/// Dropping the pipeline handle will decrement the updown counter
/// Clone MUST NOT be implemented for this type. Cloning will make extra copies that when dropped will throw off the count.
pub(crate) struct PipelineHandle {
    pub(crate) pipeline_ref: Arc<PipelineRef>,
    _guard: UpDownCounterGuard<i64>,
}

impl PipelineHandle {
    pub(crate) fn new(schema_id: String, launch_id: Option<String>, config_hash: String) -> Self {
        let pipeline_ref = Arc::new(PipelineRef {
            schema_id: schema_id.clone(),
            launch_id: launch_id.clone(),
            config_hash: config_hash.clone(),
        });

        let guard = i64_up_down_counter_with_unit!(
            "apollo.router.pipelines",
            "Number of active router pipelines",
            "{pipeline}",
            1,
            "schema.id" = schema_id,
            "launch.id" = launch_id.unwrap_or_default(),
            "config.hash" = config_hash
        );

        PipelineHandle {
            pipeline_ref,
            _guard: guard,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::FutureMetricsExt;

    #[tokio::test]
    async fn test_pipeline_handle_increments_counter() {
        async {
            let _handle = PipelineHandle::new(
                "schema1".to_string(),
                Some("launch1".to_string()),
                "config1".to_string(),
            );

            assert_up_down_counter!(
                "apollo.router.pipelines",
                1,
                "schema.id" = "schema1",
                "launch.id" = "launch1",
                "config.hash" = "config1"
            );
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn test_pipeline_handle_decrements_on_drop() {
        async {
            {
                let _handle = PipelineHandle::new(
                    "schema1".to_string(),
                    Some("launch1".to_string()),
                    "config1".to_string(),
                );

                assert_up_down_counter!(
                    "apollo.router.pipelines",
                    1,
                    "schema.id" = "schema1",
                    "launch.id" = "launch1",
                    "config.hash" = "config1"
                );
            }

            // After dropping, counter should be back to 0
            assert_up_down_counter!(
                "apollo.router.pipelines",
                0,
                "schema.id" = "schema1",
                "launch.id" = "launch1",
                "config.hash" = "config1"
            );
        }
        .with_metrics()
        .await;
    }

    #[tokio::test]
    async fn test_pipeline_handle_multiple_pipelines() {
        async {
            let _handle1 = PipelineHandle::new(
                "schema1".to_string(),
                Some("launch1".to_string()),
                "config1".to_string(),
            );

            let _handle2 = PipelineHandle::new("schema2".to_string(), None, "config2".to_string());

            // Check first pipeline
            assert_up_down_counter!(
                "apollo.router.pipelines",
                1,
                "schema.id" = "schema1",
                "launch.id" = "launch1",
                "config.hash" = "config1"
            );

            // Check second pipeline (no launch_id)
            assert_up_down_counter!(
                "apollo.router.pipelines",
                1,
                "schema.id" = "schema2",
                "launch.id" = "",
                "config.hash" = "config2"
            );
        }
        .with_metrics()
        .await;
    }
}
