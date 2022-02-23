use crate::{SubgraphRequest, SubgraphResponse};
use async_trait::async_trait;
use once_cell::sync::Lazy;
use schemars::gen::SchemaGenerator;
use schemars::JsonSchema;
use std::collections::HashMap;
use std::sync::Mutex;
use tower::util::{BoxLayer, BoxService};
use tower::BoxError;

type InstanceFactory = fn(&serde_json::Value) -> Result<BoxedSubgraphLayer, BoxError>;

type SchemaFactory = fn(&mut SchemaGenerator) -> schemars::schema::Schema;

#[derive(Clone)]
pub struct LayerFactory {
    instance_factory: InstanceFactory,
    schema_factory: SchemaFactory,
}

type BoxedSubgraphLayer = BoxLayer<
    BoxService<SubgraphRequest, SubgraphResponse, BoxError>,
    SubgraphRequest,
    SubgraphResponse,
    BoxError,
>;

impl LayerFactory {
    pub fn new(instance_factory: InstanceFactory, schema_factory: SchemaFactory) -> Self {
        Self {
            instance_factory,
            schema_factory,
        }
    }

    pub fn create_instance(
        &self,
        configuration: &serde_json::Value,
    ) -> Result<BoxedSubgraphLayer, BoxError> {
        (self.instance_factory)(configuration)
    }

    pub fn create_schema(&self, gen: &mut SchemaGenerator) -> schemars::schema::Schema {
        (self.schema_factory)(gen)
    }
}

static LAYER_REGISTRY: Lazy<Mutex<HashMap<String, LayerFactory>>> = Lazy::new(|| {
    let m = HashMap::new();
    Mutex::new(m)
});

pub fn register_layer(name: String, layer_factory: LayerFactory) {
    LAYER_REGISTRY
        .lock()
        .expect("Lock poisoned")
        .insert(name, layer_factory);
}

pub fn layers() -> HashMap<String, LayerFactory> {
    LAYER_REGISTRY.lock().expect("Lock poisoned").clone()
}

#[async_trait]
pub trait ConfigurableLayer: Send + Sync + 'static + Sized {
    type Config: JsonSchema;
    fn new(configuration: Self::Config) -> Result<Self, BoxError>;
}

/// Register a layer with a group and a name
/// Grouping prevent name clashes for layers, so choose something unique, like your domain name.
/// Layers will appear in the configuration as a layer property called: {group}_{name}
#[macro_export]
macro_rules! register_layer {
    ($name: literal, $value: ident) => {
        startup::on_startup! {
            let qualified_name = $name.to_string();

            $crate::register_layer(qualified_name, $crate::LayerFactory::new(|configuration| {
                let layer = $value::new(serde_json::from_value(configuration.clone())?)?;
                Ok(tower::util::BoxLayer::new(layer))
            }, |gen| gen.subschema_for::<<$value as $crate::ConfigurableLayer>::Config>()));
        }
    };
    ($group: literal, $name: literal, $value: ident) => {
        $crate::reexports::startup::on_startup! {
            let qualified_name = if $group == "" {
                $name.to_string()
            }
            else {
                format!("{}_{}", $group, $name)
            };

            $crate::register_layer(qualified_name, $crate::LayerFactory::new(|configuration| {
                let layer = <$value as ConfigurableLayer>::new($crate::reexports::serde_json::from_value(configuration.clone())?)?;
                Ok(tower::util::BoxLayer::new(layer))
            }, |gen| gen.subschema_for::<<$value as $crate::ConfigurableLayer>::Config>()));
        }
    };
}
