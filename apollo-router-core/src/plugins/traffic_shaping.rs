use std::collections::HashMap;

use schemars::JsonSchema;
use serde::Deserialize;
use tower::util::BoxService;
use tower::{BoxError, ServiceBuilder, ServiceExt};

use crate::deduplication::QueryDeduplicationLayer;
use crate::plugin::Plugin;
use crate::{register_plugin, SubgraphRequest, SubgraphResponse};

#[derive(PartialEq, Debug, Clone, Deserialize, JsonSchema)]
struct Shaping {
    dedup: Option<bool>,
}

impl Shaping {
    fn merge(&self, fallback: Option<&Shaping>) -> Shaping {
        match fallback {
            None => self.clone(),
            Some(fallback) => Shaping {
                dedup: self.dedup.or(fallback.dedup),
            },
        }
    }
}

#[derive(PartialEq, Debug, Clone, Deserialize, JsonSchema)]
struct Config {
    #[serde(default)]
    all: Option<Shaping>,
    #[serde(default)]
    subgraphs: HashMap<String, Shaping>,
}

struct TrafficShaping {
    config: Config,
}

#[async_trait::async_trait]
impl Plugin for TrafficShaping {
    type Config = Config;

    fn new(config: Self::Config) -> Result<Self, BoxError> {
        Ok(Self { config })
    }

    fn subgraph_service(
        &mut self,
        name: &str,
        service: BoxService<SubgraphRequest, SubgraphResponse, BoxError>,
    ) -> BoxService<SubgraphRequest, SubgraphResponse, BoxError> {
        // Either we have the subgraph config and we merge it with the all config, or we just have the all config or we have nothing.
        let all_config = self.config.all.as_ref();
        let subgraph_config = self.config.subgraphs.get(name);
        let final_config = Self::merge_config(all_config, subgraph_config);

        if let Some(config) = final_config {
            ServiceBuilder::new()
                .option_layer(config.dedup.unwrap_or_default().then(|| {
                    //Buffer is required because dedup layer requires a clone service.
                    ServiceBuilder::new()
                        .layer(QueryDeduplicationLayer::default())
                        .buffer(20_000)
                }))
                .service(service)
                .boxed()
        } else {
            service
        }
    }
}

impl TrafficShaping {
    fn merge_config(
        all_config: Option<&Shaping>,
        subgraph_config: Option<&Shaping>,
    ) -> Option<Shaping> {
        let merged_subgraph_config = subgraph_config.map(|c| c.merge(all_config));
        merged_subgraph_config.or_else(|| all_config.cloned())
    }
}

register_plugin!("experimental", "traffic_shaping", TrafficShaping);

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_merge_config() {
        let config = serde_yaml::from_str::<Config>(
            r#"
        all:
          dedup: true
        subgraphs: 
          products:
            dedup: false
        "#,
        )
        .unwrap();

        assert_eq!(TrafficShaping::merge_config(None, None), None);
        assert_eq!(
            TrafficShaping::merge_config(config.all.as_ref(), None),
            config.all
        );
        assert_eq!(
            TrafficShaping::merge_config(config.all.as_ref(), config.subgraphs.get("products"))
                .as_ref(),
            config.subgraphs.get("products")
        );

        assert_eq!(
            TrafficShaping::merge_config(None, config.subgraphs.get("products")).as_ref(),
            config.subgraphs.get("products")
        );
    }
}
