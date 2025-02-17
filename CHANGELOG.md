# Changelog

This project adheres to [Semantic Versioning v2.0.0](https://semver.org/spec/v2.0.0.html).

> [!TIP]
> All notable changes to Router v2.x after its initial release will be documented in this file.  To see previous history, see the [changelog prior to v2.0.0](https://github.com/apollographql/router/blob/1.x/CHANGELOG.md).

# [2.0.0] - 2025-02-17

This is a major release of the router containing significant new functionality and improvements to behaviour, resulting in more predictable resource utilisation and decreased latency.

Router 2.0.0 introduces general availability of Apollo Connectors, helping integrate REST services in router deployments.

This entry summarizes the overall changes in 2.0.0. To learn more details, go to the [What's New in router v2.x](https://www.apollographql.com/docs/graphos/routing/about-v2) page.

To upgrade to this version, follow the [upgrading from router 1.x to 2.x](https://www.apollographql.com/docs/graphos/reference/upgrade/from-router-v1) guide.

## ❗ BREAKING CHANGES ❗

In order to make structural improvements in the router and upgrade some of our key dependencies, some breaking changes were introduced in this major release. Most of the breaking changes are in the areas of configuration and observability. All details on what's been removed and changed can be found in the [upgrade guide](https://www.apollographql.com/docs/graphos/reference/upgrade/from-router-v1).

## 🚀 Features

Router 2.0.0 comes with many new features and improvements. While all the details can be found in the [What's New guide](https://www.apollographql.com/docs/graphos/routing/about-v2), the following features are the ones we are most excited about.

**Simplified integration of REST services using Apollo Connectors.** Apollo Connectors are a declarative programming model for GraphQL, allowing you to plug your existing REST services directly into your graph. Once integrated, client developers gain all the benefits of GraphQL, and API owners gain all the benefits of GraphOS, including incorporation into a supergraph for a comprehensive, unified view of your organization's data and services. [This detailed guide](https://www.apollographql.com/docs/graphos/schema-design/connectors/router) outlines how to configure connectors with the router.  Moving from Connectors Preview can be accomplished by following the steps in the [Connectors GA upgrade guide](https://www.apollographql.com/docs/graphos/schema-design/connectors/changelog).

**Predictable resource utilization and availability with back pressure.** Back pressure was not maintained in router 1.x, which meant _all_ requests were being accepted by the router. This resulted in issues for routers which are accepting high levels of traffic. Router 2.0.0 improves the handling of back pressure so that traffic shaping measures are more effective while also improving integration with telemetry. Improvements to back pressure then allows for significant improvements in traffic shaping, which improves router's ability to observe timeout and traffic shaping restrictions correctly. You can read about traffic shaping changes in [this section of the upgrade guide](https://www.apollographql.com/docs/graphos/reference/upgrade/from-router-v1#traffic-shaping).

**Metrics now all follow OpenTelemetry naming conventions.** Some of router's earlier metrics were created before the introduction of OpenTelemetry, resulting in naming inconsistencies. Along with standardising metrics to OpenTelemetry, Apollo operation usage reporting now also defaults to using OpenTelemetry in router 2.0.0. Quite a few existing metrics had to be changed in order to do this properly and correctly, and we encourage you to carefully read through the upgrade guide for all the metrics changes.

**Improved validation of CORS configurations, preventing silent failures.** While CORS configuration did not change in router 2.0.0, we did improve CORS value validation. This results in things like invalid regex or unknown `allow_methods` returning errors early and preventing starting the router.

**Documentation for context keys, improving usability for advanced customers.** Router 2.0.0 creates consistent naming semantics for request context keys, which are used to share data across internal router pipeline stages. If you are relying on context entries in rust plugins, rhai scripts, coprocessors, or telemetry selectors, please refer to [this section](https://www.apollographql.com/docs/graphos/reference/upgrade/from-router-v1#context-keys) to see what keys changed.

## 📃 Configuration

Some changes to router configuration options were necessary in this release. Descriptions for both breaking changes to previous configuration and configuration for new features can be found in the [upgrade guide](https://www.apollographql.com/docs/graphos/reference/upgrade/from-router-v1)).

## 🛠 Maintenance

Many external Rust dependencies (crates) have been updated to modern versions where possible. As the Rust ecosystem evolves, so does the router. Keeping these crates up to date helps keep the router secure and stable.

Major upgrades in this version include:

- `axum`
- `http`
- `hyper`
- `opentelemetry`
- `redis`
