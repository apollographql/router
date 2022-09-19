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
## 🛠 Maintenance

### Add more compilation gates to delete useless warnings ([PR #1830](https://github.com/apollographql/router/pull/1830))

Add more gates (for `console` feature) to not have warnings when using `--all-features`.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/1830

## 📚 Documentation
