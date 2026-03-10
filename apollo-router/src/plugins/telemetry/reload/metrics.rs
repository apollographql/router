//! Metrics provider construction
//!
//! This module provides tools for building OpenTelemetry meter providers from router configuration.
//!
//! ## Purpose
//!
//! The [`MetricsBuilder`] constructs meter providers for different telemetry backends:
//! - **Public metrics** - Prometheus and OTLP exporters for general observability
//! - **Apollo metrics** - Special meter providers for Apollo Studio reporting
//!
//! ## Configurator Pattern
//!
//! The [`MetricsConfigurator`] trait allows different metric exporters to contribute to the
//! builder in a uniform way. Each exporter (Prometheus, OTLP, Apollo) implements this trait
//! to extract its configuration and add appropriate readers and views to the builder.
//!
//! ## Provider Types
//!
//! Multiple meter providers are created to serve different purposes:
//! - `Public` - For standard metrics exposed via Prometheus or sent to OTLP collectors
//! - `Apollo`/`ApolloRealtime` - For metrics sent to Apollo Studio with specific filtering

use std::collections::HashSet;

use ahash::HashMap;
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::metrics::Aggregation;
use opentelemetry_sdk::metrics::Instrument;
use opentelemetry_sdk::metrics::InstrumentKind;
use opentelemetry_sdk::metrics::MeterProviderBuilder;
use opentelemetry_sdk::metrics::SdkMeterProvider;
use opentelemetry_sdk::metrics::Stream;
use prometheus::Registry;
use tower::BoxError;

use crate::_private::telemetry::ConfigResource;
use crate::metrics::aggregation::MeterProviderType;
use crate::metrics::filter::FilterMeterProvider;
use crate::plugins::telemetry::apollo_exporter::Sender;
use crate::plugins::telemetry::config::Conf;
use crate::plugins::telemetry::config::MetricView;
use crate::plugins::telemetry::config::MetricsCommon;

/// Trait for metric exporters to contribute to meter provider construction
pub(crate) trait MetricsConfigurator {
    fn config(conf: &Conf) -> &Self;
    fn is_enabled(&self) -> bool;
    fn configure<'a>(&self, builder: &mut MetricsBuilder<'a>) -> Result<(), BoxError>;
}

/// Builder for constructing OpenTelemetry meter providers.
///
/// Accumulates configuration from multiple exporters and builds the final meter providers
/// with appropriate readers, views, and resource attributes.
pub(crate) struct MetricsBuilder<'a> {
    pub(super) meter_provider_builders:
        HashMap<MeterProviderType, opentelemetry_sdk::metrics::MeterProviderBuilder>,
    /// Tracks which provider types have readers configured.
    /// Only providers with readers are returned from build() to avoid OTel SDK errors
    /// when observable instruments are registered with providers that have no readers.
    providers_with_readers: HashSet<MeterProviderType>,
    apollo_metrics_sender: Sender,
    prometheus_registry: Option<Registry>,
    metrics_common: &'a MetricsCommon,
    resource: Resource,
}

impl<'a> MetricsBuilder<'a> {
    pub(crate) fn build(
        self,
    ) -> (
        Option<Registry>,
        HashMap<MeterProviderType, FilterMeterProvider>,
        Sender,
    ) {
        (
            self.prometheus_registry,
            self.meter_provider_builders
                .into_iter()
                // Only include providers that have readers configured.
                // Providers with only views but no readers would cause OTel SDK to emit
                // errors when observable instruments are registered.
                .filter(|(k, _)| self.providers_with_readers.contains(k))
                .map(|(k, v)| {
                    (
                        k,
                        match k {
                            MeterProviderType::Public => FilterMeterProvider::public(v.build()),
                            MeterProviderType::OtelDefault => {
                                FilterMeterProvider::public(v.build())
                            }
                            MeterProviderType::Apollo => FilterMeterProvider::apollo(v.build()),
                            MeterProviderType::ApolloRealtime => {
                                FilterMeterProvider::apollo_realtime(v.build())
                            }
                        },
                    )
                })
                .collect(),
            self.apollo_metrics_sender,
        )
    }
    pub(crate) fn configure<T: MetricsConfigurator>(&mut self, config: &T) -> Result<(), BoxError> {
        if config.is_enabled() {
            return config.configure(self);
        }
        Ok(())
    }

    pub(crate) fn new(config: &'a Conf) -> Self {
        let resource = config.exporters.metrics.common.to_resource();

        Self {
            meter_provider_builders: HashMap::default(),
            providers_with_readers: HashSet::new(),
            resource,
            apollo_metrics_sender: Sender::default(),
            prometheus_registry: None,
            metrics_common: &config.exporters.metrics.common,
        }
    }
    pub(crate) fn metrics_common(&self) -> &MetricsCommon {
        self.metrics_common
    }
    pub(crate) fn with_prometheus_registry(&mut self, prometheus_registry: Registry) -> &mut Self {
        self.prometheus_registry = Some(prometheus_registry);
        self
    }
    pub(crate) fn with_apollo_metrics_sender(
        &mut self,
        apollo_metrics_sender: Sender,
    ) -> &mut Self {
        self.apollo_metrics_sender = apollo_metrics_sender;
        self
    }
    pub(crate) fn with_reader<T: opentelemetry_sdk::metrics::reader::MetricReader>(
        &mut self,
        meter_provider_type: MeterProviderType,
        reader: T,
    ) -> &mut Self {
        let meter_provider = self.meter_provider(meter_provider_type);
        *meter_provider = std::mem::take(meter_provider).with_reader(reader);
        // Track that this provider has a reader - only providers with readers will be returned
        self.providers_with_readers.insert(meter_provider_type);
        self
    }

    pub(crate) fn with_view<T>(
        &mut self,
        meter_provider_type: MeterProviderType,
        view: T,
    ) -> &mut Self
    where
        T: Fn(&Instrument) -> Option<Stream> + Send + Sync + 'static,
    {
        let meter_provider = self.meter_provider(meter_provider_type);
        *meter_provider = std::mem::take(meter_provider).with_view(view);
        self
    }

    pub(crate) fn with_resource(
        &mut self,
        meter_provider_type: MeterProviderType,
        resource: Resource,
    ) -> &mut Self {
        let meter_provider = self.meter_provider(meter_provider_type);
        *meter_provider = std::mem::take(meter_provider).with_resource(resource);
        self
    }

    /// Gets or creates a meter provider builder for a specific type.
    ///
    /// Note: Only Public and OtelDefault providers include resource attributes.
    /// Apollo providers omit resources as they're not sent to Apollo Studio.
    fn meter_provider(
        &mut self,
        meter_provider_type: MeterProviderType,
    ) -> &mut MeterProviderBuilder {
        self.meter_provider_builders
            .entry(meter_provider_type)
            .or_insert_with(|| match meter_provider_type {
                // Public and default providers include resource attributes (service name, etc.)
                MeterProviderType::Public => {
                    SdkMeterProvider::builder().with_resource(self.resource.clone())
                }
                MeterProviderType::OtelDefault => {
                    SdkMeterProvider::builder().with_resource(self.resource.clone())
                }
                // Apollo providers omit resource attributes - not sent to Apollo Studio
                MeterProviderType::Apollo => SdkMeterProvider::builder(),
                MeterProviderType::ApolloRealtime => SdkMeterProvider::builder(),
            })
    }

    pub(crate) fn configure_views(&mut self, meter_provider_type: MeterProviderType) {
        let boundaries = self.metrics_common().buckets.clone();
        let user_views: HashMap<String, MetricView> = self
            .metrics_common()
            .views
            .clone()
            .into_iter()
            .map(|v| (v.name.clone(), v))
            .collect();
        let user_view_names: HashSet<String> = user_views.keys().cloned().collect();

        // Merge each user-configured view with default histogram aggregation.
        // This ensures user views inherit default buckets when no custom aggregation
        // is specified, while still allowing user overrides for any field.
        for (name, user_view) in user_views {
            let default_view = MetricView::default_histogram(name, boundaries.clone());
            let merged = default_view.merge(user_view);
            self.with_view(meter_provider_type, merged.into_view_fn());
        }

        // Apply default histogram buckets to all histograms without user-configured views
        self.with_view(meter_provider_type, move |instrument: &Instrument| {
            if user_view_names.contains(instrument.name()) {
                return None;
            }
            if instrument.kind() == InstrumentKind::Histogram {
                Some(
                    Stream::builder()
                        .with_aggregation(Aggregation::ExplicitBucketHistogram {
                            boundaries: boundaries.clone(),
                            record_min_max: true,
                        })
                        .build()
                        .expect("Failed to create stream for default histogram bucket view"),
                )
            } else {
                None
            }
        });
    }
}
