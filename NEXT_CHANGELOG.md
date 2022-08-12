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

# [0.16] (unreleased) - 2022-mm-dd

## ❗ BREAKING ❗

### Removed `delay_interval` in telemetry configuration. [PR #FIXME]

It was doing nothing.

```yaml title="router.yaml"
telemetry:
  metrics:
    common:
      # Removed, will now cause an error on Router startup:
      delay_interval:
        secs: 9
        nanos: 500000000
```

By [@SimonSapin](https://github.com/SimonSapin)

### Remove telemetry configuration hot reloading ([PR #1463](https://github.com/apollographql/router/pull/1463))

Configuration hot reloading is not very useful for telemetry, and is the
source of regular bugs that are hard to fix.

This removes the support for configuration reloading entirely. Now, the
router will reject a configuration reload with an error log if the
telemetry configuration changed.

It is now possible to create a subscriber and pass it explicitely to the telemetry plugin
when creating it. It will then be modified to integrate the telemetry plugin's layer.

By [@geal](https://github.com/geal) in https://github.com/apollographql/router/pull/1463

## 🚀 Features

### Expose query plan in extensions for GraphQL response (experimental) ([PR #1470](https://github.com/apollographql/router/pull/1470))

Expose query plan in extensions for GraphQL response. Only experimental for now, no documentation available.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/1470

### Add support of global rate limit and timeout. [PR #1347](https://github.com/apollographql/router/pull/1347)

Additions to the traffic shaping plugin:
- **Global rate limit** - If you want to rate limit requests to subgraphs or to the router itself.
- **Timeout**: - Set a timeout to subgraphs and router requests.

```yaml
traffic_shaping:
  router: # Rules applied to requests from clients to the router
    global_rate_limit: # Accept a maximum of 10 requests per 5 secs. Excess requests must be rejected.
      capacity: 10
      interval: 5s # Must not be greater than 18_446_744_073_709_551_615 milliseconds and not less than 0 milliseconds
    timeout: 50s # If a request to the router takes more than 50secs then cancel the request (30 sec by default)
  subgraphs: # Rules applied to requests from the router to individual subgraphs
    products:
      global_rate_limit: # Accept a maximum of 10 requests per 5 secs from the router. Excess requests must be rejected.
        capacity: 10
        interval: 5s # Must not be greater than 18_446_744_073_709_551_615 milliseconds and not less than 0 milliseconds
      timeout: 50s # If a request to the subgraph 'products' takes more than 50secs then cancel the request (30 sec by default)
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/1347

## 🐛 Fixes

## 🛠 Maintenance

## 📚 Documentation
