use async_trait::async_trait;
use std::{collections::HashMap, sync::Arc};
use tracing::Instrument;

use self::configuration::HookPointKind;
use crate::Request;

pub mod configuration;

pub struct Extensions {
    // hook point kind -> extension name
    hooks: HashMap<HookPointKind, String>,

    extensions: HashMap<String, Arc<dyn ExtensionFactory>>,
}

impl std::fmt::Debug for Extensions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Extensions")
            .field("hooks", &self.hooks)
            .finish()
    }
}

pub struct SessionContext {
    trace_id: u64,
}

impl Extensions {
    pub fn new() -> Self {
        Extensions {
            hooks: HashMap::new(),
            extensions: HashMap::new(),
        }
    }

    pub fn add_extension(&mut self, name: String, config: self::configuration::Extension) {
        unimplemented!()
    }
}

// everything here
impl Extensions {
    pub async fn lookup_extension(
        &self,
        session_context: Option<SessionContext>,
        kind: HookPointKind,
    ) -> Option<(String, Arc<dyn ExtensionInstance>)> {
        let name = self.hooks.get(&kind)?;
        let factory = self.extensions.get(name)?;

        factory
            .get_instance(session_context, kind)
            .await
            .map(|ext| (name.to_string(), ext))
    }

    pub async fn requestDidStart(&self, request: &Request) {
        // lookup the trace id from the request
        // ...
        let ctx = SessionContext { trace_id: 0 };

        if let Some((name, instance)) = self
            .lookup_extension(Some(ctx), HookPointKind::RequestDidStart)
            .await
        {
            instance
                .requestDidStart(request)
                .instrument(tracing::info_span!(
                    "requestDidStart",
                    extension = name.as_str()
                ))
                .await
        }
    }
}

#[async_trait]
pub trait ExtensionFactory: Send + Sync {
    async fn get_instance(
        &self,
        session_context: Option<SessionContext>,
        kind: HookPointKind,
    ) -> Option<Arc<dyn ExtensionInstance>>;

    async fn close(&self);
}

#[async_trait]
pub trait ExtensionInstance: Send + Sync {
    async fn requestDidStart(&self, request: &Request);

    // when do we call that?
    async fn close(&self);
}
