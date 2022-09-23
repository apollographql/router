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

### Do not erase errors when missing `_entities` ([Issue #1863](https://github.com/apollographql/router/issues/1863))

in a federated query, if the subgraph returned a response with errors and a null or absent data field, the router
was ignoring the subgraph error and instead returning an error complaining about the missing` _entities` field.
This will now aggregate the subgraph error and the missing `_entities` error.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/1870

## 🛠 Maintenance

### Have CI use rust-toolchain.toml and not install another redudant toolchain ([Issue #1313](https://github.com/apollographql/router/issues/1313))

Avoids redundant work in CI and makes the YAML configuration less mis-leading.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/1877

### Query plan execution refactoring ([PR #1843](https://github.com/apollographql/router/pull/1843))

This splits the query plan execution in multiple modules to make the code more manageable.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/1843

### Remove `Buffer` from APQ ([PR #1641](https://github.com/apollographql/router/pull/1641))

This removes `tower::Buffer` usage from the Automated Persisted Queries implementation to improve reliability.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/1641

## 📚 Documentation
