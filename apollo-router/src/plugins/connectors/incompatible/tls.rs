use std::collections::HashSet;

use super::ConfiguredSubgraphs;
use super::IncompatiblePlugin;
use crate::Configuration;
use crate::configuration::Tls;

pub(super) struct TlsIncompatPlugin {
    config: Tls,
}

impl TlsIncompatPlugin {
    pub(super) fn from_config(config: &Configuration) -> Option<Self> {
        // TLS is always enaabled and gets default initialized
        Some(Self {
            config: config.tls.clone(),
        })
    }
}

impl IncompatiblePlugin for TlsIncompatPlugin {
    fn is_applied_to_all(&self) -> bool {
        let all = &self.config.subgraph.all;

        // Since everything gets default initialized, we need to manually check
        // that every field is not set :(
        all.certificate_authorities.is_some() || all.client_authentication.is_some()
    }

    fn configured_subgraphs(&self) -> super::ConfiguredSubgraphs<'_> {
        // TLS cannot be manually disabled per subgraph, so all configured are
        // enabled.
        ConfiguredSubgraphs {
            enabled: self.config.subgraph.subgraphs.keys().collect(),
            disabled: HashSet::with_hasher(Default::default()),
        }
    }

    fn inform_incompatibilities(
        &self,
        subgraphs: std::collections::HashSet<&String>,
        _connectors: &apollo_federation::connectors::expand::Connectors,
    ) {
        for subgraph in subgraphs {
            if self.config.subgraph.subgraphs.contains_key(subgraph) {
                tracing::warn!(
                    subgraph = subgraph,
                    message = "The `tls` plugin is explicitly configured for a subgraph containing connectors, which is not supported. Instead, configure the connector sources directly using `tls.connector.sources.<subgraph_name>.<source_name>`.",
                    see = "https://www.apollographql.com/docs/graphos/schema-design/connectors/router#tls",
                );
            }
        }
    }
}
