# Changelog for the next release

All notable changes to Router will be documented in this file.

This project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

<!-- <THIS IS AN EXAMPLE, DO NOT REMOVE>

# [x.x.x] (unreleased) - 2022-mm-dd
> Important: X breaking changes below, indicated by **❗ BREAKING ❗**
## ❗ BREAKING ❗
## 🚀 Features ( :rocket: )
## 🐛 Fixes ( :bug: )
## 🛠 Maintenance ( :hammer_and_wrench: )
## 📚 Documentation ( :books: )
## 🐛 Fixes ( :bug: )

## Example section entry format

- **Headline** ([PR #PR_NUMBER](https://github.com/apollographql/router/pull/PR_NUMBER))

  Description! And a link to a [reference](http://url)
-->

# [v0.1.0-preview.8] - (unreleased)
## ❗ BREAKING ❗
## 🚀 Features ( :rocket: )
## 🐛 Fixes ( :bug: )

### Improve the configuration error report [PR #963](https://github.com/apollographql/router/pull/963)
In case you have unknown properties on your configuration it will highlight the entity with unknown properties. Before we always pointed on the first field of this entity even if it wasn't the bad one, it's now fixed.

### Fix incorrectly omitting content of interface's fragment [PR #949](https://github.com/apollographql/router/pull/949)
Router now distinguish between fragment on concrete type and interface.
If interface is encountered and  `__typename` is queried, additionally checks that returned type implements interface.

### Set the service name if not specified in config or environment [PR #960](https://github.com/apollographql/router/pull/960)
The router now sets "router" as default service name in Opentelemetry traces, that can be replaced using the configuration file or environment variables. It also sets the key "process.executable_name".
## 🛠 Maintenance ( :hammer_and_wrench: )
## 📚 Documentation ( :books: )
