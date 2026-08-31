//! Test doubles shared by the reload modules.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;

use opentelemetry::Context;
use opentelemetry_sdk::error::OTelSdkResult;
use opentelemetry_sdk::trace::SpanData;
use opentelemetry_sdk::trace::SpanProcessor;

/// Records whether the provider it was installed on propagated a shutdown down to its span
/// processors.
#[derive(Debug, Default, Clone)]
pub(in crate::plugins::telemetry::reload) struct ShutdownProbe {
    shut_down: Arc<AtomicBool>,
}

impl ShutdownProbe {
    pub(in crate::plugins::telemetry::reload) fn was_shut_down(&self) -> bool {
        self.shut_down.load(Ordering::SeqCst)
    }
}

impl SpanProcessor for ShutdownProbe {
    fn on_start(&self, _span: &mut opentelemetry_sdk::trace::Span, _cx: &Context) {}

    fn on_end(&self, _span: SpanData) {}

    fn force_flush(&self) -> OTelSdkResult {
        Ok(())
    }

    fn shutdown_with_timeout(&self, _timeout: Duration) -> OTelSdkResult {
        self.shut_down.store(true, Ordering::SeqCst);
        Ok(())
    }
}
