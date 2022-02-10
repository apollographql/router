use crate::{SubgraphRequest, SubgraphResponse};
use async_trait::async_trait;
use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use tower::util::{BoxLayer, BoxService};
use tower::BoxError;

type LayerFactory = fn(
    &serde_json::Value,
) -> Result<
    BoxLayer<
        BoxService<SubgraphRequest, SubgraphResponse, BoxError>,
        SubgraphRequest,
        SubgraphResponse,
        BoxError,
    >,
    BoxError,
>;

static LAYER_REGISTRY: Lazy<Mutex<HashMap<String, LayerFactory>>> = Lazy::new(|| {
    let m = HashMap::new();
    Mutex::new(m)
});

pub fn layers() -> Arc<HashMap<String, LayerFactory>> {
    Arc::new(LAYER_REGISTRY.lock().expect("Lock poisoned").clone())
}

pub fn layers_mut<'a>() -> MutexGuard<'a, HashMap<String, LayerFactory>> {
    LAYER_REGISTRY.lock().expect("Lock poisoned")
}

#[async_trait]
pub trait ConfigurableLayer: Default + Send + Sync + 'static {
    type Config;

    fn configure(&mut self, _configuration: Self::Config) -> Result<(), BoxError> {
        Ok(())
    }
}

// Register a layer with a name
#[macro_export]
macro_rules! register_layer {
    ($key: literal, $value: ident) => {
        startup::on_startup! {
            // Register the plugin factory function
            $crate::layer::layers_mut().insert($key.to_string(), |configuration| {
                let mut layer = $value::default();
                let typed_configuration = serde_json::from_value(configuration.clone())?;
                layer.configure(typed_configuration)?;
                Ok(tower::util::BoxLayer::new(layer))
            });
        }
    };
}
