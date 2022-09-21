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
## 🚀 Features

### Add support of dynamic header injection ([Issue #1755](https://github.com/apollographql/router/issues/1755))

+ Insert static header

```yaml
headers:
  all: # Header rules for all subgraphs
    request:
    - insert:
        name: "sent-from-our-apollo-router"
        value: "indeed"
```

+ Insert header from context

```yaml
headers:
  all: # Header rules for all subgraphs
    request:
    - insert:
        name: "sent-from-our-apollo-router-context"
        from_context: "my_key_in_context"
```

+ Insert header from request body

```yaml
headers:
  all: # Header rules for all subgraphs
    request:
    + insert:
        name: "sent-from-our-apollo-router-request-body"
        path: ".operationName" # It's a JSON path query to fetch the operation name from request body
        default: "UNKNOWN" # If no operationName has been specified
```


By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/1830

## 🐛 Fixes

### update and validate configuration files ([Issue #1854](https://github.com/apollographql/router/issues/1854))

Several of the dockerfiles in the router repository were out of date with respect to recent configuration changes. This fix extends our configuration testing range and updates the configuration files.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/1857

## 🛠 Maintenance

### Disable Deno snapshotting on docs.rs

This works around [V8 linking errors](https://docs.rs/crate/apollo-router/1.0.0-rc.2/builds/633287).

By [@SimonSapin](https://github.com/SimonSapin) in https://github.com/apollographql/router/pull/1847

### Add the Uplink schema to the repository, with a test checking that it is up to date.

Previously it was downloaded at compile-time, 
which would [fail](https://docs.rs/crate/lets-see-if-this-builds-on-docs-rs/0.0.1/builds/633305) 
in build environments without Internet access.
If an update is needed, the test failure prints a message with the command to run.

By [@SimonSapin](https://github.com/SimonSapin) in https://github.com/apollographql/router/pull/1847

## 📚 Documentation
