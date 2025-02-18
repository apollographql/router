use std::collections::HashSet;

use super::ConfiguredSubgraphs;
use super::IncompatiblePlugin;
use crate::configuration::Tls;
use crate::Configuration;

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
        _connectors: &apollo_federation::sources::connect::expand::Connectors,
    ) {
        for subgraph in subgraphs {
            if self.config.subgraph.subgraphs.contains_key(subgraph) {
                tracing::warn!(
                    subgraph = subgraph,
                    message = "plugin `tls` is explicitly configured for connector-enabled subgraph, which is not supported",
                    see = "https://go.apollo.dev/connectors/incompat",
                );
            } else {
                tracing::info!(
                    subgraph = subgraph,
                    message = "plugin `tls` indirectly targets a connector-enabled subgraph, which is not supported",
                    see = "https://go.apollo.dev/connectors/incompat",
                );
            }
        }
    }
}
