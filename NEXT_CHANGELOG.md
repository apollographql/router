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
### Span client_name and client_version attributes renamed ([#1514](https://github.com/apollographql/router/issues/1514))
OpenTelemetry attributes should be grouped by `.` rather than `_`, therefore the following attributes have changed:

* `client_name` => `client.name`
* `client_version` => `client.version`

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/1514

## 🚀 Features

### Add federated tracing support to Apollo studio usage reporting ([#1514](https://github.com/apollographql/router/issues/1514))

Add support of [federated tracing](https://www.apollographql.com/docs/federation/metrics/) in Apollo Studio:

```yaml
telemetry:
    apollo:
        # The percentage of requests will include HTTP request and response headers in traces sent to Apollo Studio.
        # This is expensive and should be left at a low value.
        # This cannot be higher than tracing->trace_config->sampler
        field_level_instrumentation_sampler: 0.01 # (default)
        
        # Include HTTP request and response headers in traces sent to Apollo Studio
        send_headers: # other possible values are all, only (with an array), except (with an array), none (by default)
            except: # Send all headers except referer
            - referer

        # Send variable values in Apollo in traces sent to Apollo Studio
        send_variable_values: # other possible values are all, only (with an array), except (with an array), none (by default)
            except: # Send all variable values except for variable named first
            - first
    tracing:
        trace_config:
            sampler: 0.5 # The percentage of requests that will generate traces (a rate or `always_on` or `always_off`)
```

By [@BrynCooke](https://github.com/BrynCooke) & [@bnjjj](https://github.com/bnjjj) & [@o0Ignition0o](https://github.com/o0Ignition0o) in https://github.com/apollographql/router/pull/1514


## 🐛 Fixes
## 🛠 Maintenance
## 📚 Documentation
