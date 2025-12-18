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

use ahash::HashMap;
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::metrics::{Aggregation, Instrument, InstrumentKind};
use opentelemetry_sdk::metrics::MeterProviderBuilder;
use opentelemetry_sdk::metrics::SdkMeterProvider;
use opentelemetry_sdk::metrics::Stream;
use prometheus::Registry;
use tower::BoxError;

use crate::_private::telemetry::ConfigResource;
use crate::metrics::aggregation::MeterProviderType;
use crate::metrics::filter::FilterMeterProvider;
use crate::plugins::telemetry::apollo_exporter::Sender;
use crate::plugins::telemetry::config::{Conf, MetricAggregation};
use crate::plugins::telemetry::config::MetricsCommon;
use crate::plugins::telemetry::config::OTelMetricView;

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

    pub(crate) fn configure_views(
        &mut self,
        meter_provider_type: MeterProviderType,
    ) -> Result<(), BoxError> {
        // First apply a "common" view with buckets for those that don't have a custom view defined
        let instrument_names_with_custom_views = self
            .metrics_common()
            .views
            .iter()
            .map(|v| v.name.clone())
            .collect::<Vec<_>>();
        let common_buckets = self.metrics_common().buckets.clone();
        let common_view = move |i: &Instrument| {
            if matches!(i.kind(), InstrumentKind::Histogram) && !instrument_names_with_custom_views.contains(&i.name().to_string()) {
                Some(
                    Stream::builder()
                        .with_aggregation(Aggregation::ExplicitBucketHistogram {
                            boundaries: common_buckets.clone(),
                            record_min_max: true,
                        })
                        .build()
                        .unwrap(),
                )
            } else {
                None
            }
        };
        self.with_view(meter_provider_type, common_view);
        // Next apply all custom views. If new buckets are not defined by the custom view, we add
        // the common buckets to it.
        for metric_view in self.metrics_common().views.clone() {
            // MetricView doesn't have access to metric_common, so we insert the default via hook
            // here.
            let view;
            let common_buckets = self.metrics_common().buckets.clone();
            if metric_view.aggregation.is_none() {
                view = metric_view.try_into_otel_metric_view_with(move |i, builder| {
                    if matches!(i.kind(), InstrumentKind::Histogram) {
                        builder.with_aggregation(
                            Aggregation::ExplicitBucketHistogram {
                                boundaries: common_buckets.clone(),
                                record_min_max: true,
                            }
                        )
                    } else {
                        builder
                    }
                })?;
            } else {
                view = metric_view.try_into()?;
            }

            self.with_view(meter_provider_type, view);
        }
        Ok(())
    }
}
