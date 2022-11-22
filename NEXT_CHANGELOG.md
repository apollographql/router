# Changelog for the next release

All notable changes to Router will be documented in this file.

This project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

<!-- <THIS IS AN EXAMPLE, DO NOT REMOVE>

# [x.x.x] (unreleased) - 2022-mm-dd
> Important: X breaking changes below, indicated by **❗ BREAKING ❗**
## ❗ BREAKING ❗
## 🚀 Features
## 🐛 Fixes
## 🛠 Maintenance
## 📚 Documentation

## Example section entry format

### Headline ([Issue #ISSUE_NUMBER](https://github.com/apollographql/router/issues/ISSUE_NUMBER))

Description! And a link to a [reference](http://url)

By [@USERNAME](https://github.com/USERNAME) in https://github.com/apollographql/router/pull/PULL_NUMBER
-->

# [x.x.x] (unreleased) - 2022-mm-dd
## ❗ BREAKING ❗
### Fix naming inconsistency of telemetry.metrics.common.attributes.router ([Issue #2076](https://github.com/apollographql/router/issues/2076))

Mirroring the rest of the config `router` should be `supergraph`

```yaml
telemetry:
  metrics:
    common:
      attributes:
        router: # old
```
becomes
```yaml
telemetry:
  metrics:
    common:
      attributes:
        supergraph: # new
```

By [@bryncooke](https://github.com/bryncooke) in https://github.com/apollographql/router/pull/2116

### CLI structure changes ([Issue #2076](https://github.com/apollographql/router/issues/2123))

As the Router gains functionality the limitations of the current CLI structure are becoming apparent.

There is now a separate subcommand for config related operations:
* `config`
  * `schema` - Output the configuration schema
  * `upgrade` - Upgrade the configuration with optional diff support.

`router --schema` has been deprecated and users should move to `router config schema`.

## 🚀 Features

### Configuration upgrades ([Issue #2076](https://github.com/apollographql/router/issues/2123))

Occasionally we will make changes to the Router yaml configuration format. 
When starting the Router if the configuration can be upgraded it will do so automatically and display a warning:

```
2022-11-22T14:01:46.884897Z  WARN router configuration contains deprecated options: 

  1. telemetry.tracing.trace_config.attributes.router has been renamed to 'supergraph' for consistency

These will become errors in the future. Run `router config upgrade <path_to_router.yaml>` to see a suggested upgraded configuration.
```

Note: If a configuration has errors after upgrading then the configuration will not be upgraded automatically.

From the CLI users can run:
* `router config upgrade <path_to_router.yaml>` to output configuration that has been upgraded to match the latest config format.
* `router config upgrade --diff <path_to_router.yaml>` to output a diff e.g.
```
 telemetry:
   apollo:
     client_name_header: apollographql-client-name
   metrics:
     common:
       attributes:
-        router:
+        supergraph:
           request:
             header:
             - named: "1" # foo
```

There are situations where comments and whitespace are not preserved. This may be improved in future.

By [@bryncooke](https://github.com/bryncooke) in https://github.com/apollographql/router/pull/2116

## 🐛 Fixes
## 🛠 Maintenance
## 📚 Documentation
