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

### Add `trace_id` in logs to identify all logs related to a specific request [Issue #1981](https://github.com/apollographql/router/issues/1981))

It automatically adds a `trace_id` on logs to identify which log is related to a specific request. Also adds `apollo_trace_id` in response headers to help the client to identify logs for this request.

Example of logs in text:

```logs
2022-10-21T15:17:45.562553Z ERROR [trace_id=5e6a6bda8d0dca26e5aec14dafa6d96f] apollo_router::services::subgraph_service: fetch_error="hyper::Error(Connect, ConnectError(\"tcp connect error\", Os { code: 111, kind: ConnectionRefused, message: \"Connection refused\" }))"
2022-10-21T15:17:45.565768Z ERROR [trace_id=5e6a6bda8d0dca26e5aec14dafa6d96f] apollo_router::query_planner::execution: Fetch error: HTTP fetch failed from 'accounts': HTTP fetch failed from 'accounts': error trying to connect: tcp connect error: Connection refused (os error 111)
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/1982

## 🐛 Fixes
## 🛠 Maintenance

### Split the configuration file management in multiple modules [Issue #1790](https://github.com/apollographql/router/issues/1790))

The file is becoming large and hard to modify.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/1996

## 📚 Documentation
