use std::collections::HashMap;
use std::sync::OnceLock;

use parking_lot::Mutex;
use parking_lot::MutexGuard;

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
/// Creating a new pipeline handle will insert a PipelineRef into a static map.
/// Dropping all pipeline handles associated with the internal ref will remove the PipelineRef
/// Clone MUST NOT be implemented for this type. Cloning will make extra copies that when dropped will throw off the global count.
pub(crate) struct PipelineHandle {
    pipeline_ref: PipelineRef,
}

static PIPELINES: OnceLock<Mutex<HashMap<PipelineRef, u64>>> = OnceLock::new();
pub(crate) fn pipelines() -> MutexGuard<'static, HashMap<PipelineRef, u64>> {
    PIPELINES.get_or_init(Default::default).lock()
}

impl PipelineHandle {
    pub(crate) fn new(schema_id: String, launch_id: Option<String>, config_hash: String) -> Self {
        let pipeline_ref = PipelineRef {
            schema_id: schema_id.to_string(),
            launch_id,
            config_hash: config_hash.to_string(),
        };
        pipelines()
            .entry(pipeline_ref.clone())
            .and_modify(|p| *p += 1)
            .or_insert(1);
        PipelineHandle { pipeline_ref }
    }
}

impl Clone for PipelineHandle {
    fn clone(&self) -> Self {
        pipelines()
            .entry(self.pipeline_ref.clone())
            .and_modify(|p| *p += 1)
            .or_insert(1);
        PipelineHandle {
            pipeline_ref: self.pipeline_ref.clone(),
        }
    }
}

impl Drop for PipelineHandle {
    fn drop(&mut self) {
        let mut pipelines = pipelines();
        let value = pipelines
            .get_mut(&self.pipeline_ref)
            .expect("pipeline_ref MUST be greater than zero");
        *value -= 1;
        if *value == 0 {
            pipelines.remove(&self.pipeline_ref);
        }
    }
}
