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
## 🥼 Experimental

## Example section entry format

### Headline ([Issue #ISSUE_NUMBER](https://github.com/apollographql/router/issues/ISSUE_NUMBER))

Description! And a link to a [reference](http://url)

By [@USERNAME](https://github.com/USERNAME) in https://github.com/apollographql/router/pull/PULL_NUMBER
-->

# [x.x.x] (unreleased) - 2022-mm-dd

## 🚀 Features

### Apollo uplink: Configurable schema poll timeout ([PR #2271](https://github.com/apollographql/router/pull/2271))

In addition to the url and poll interval, Uplink poll timeout can now be configured via command line arg and env variable:

```bash
        --apollo-uplink-timeout <APOLLO_UPLINK_TIMEOUT>
            The timeout for each of the polls to Apollo Uplink. [env: APOLLO_UPLINK_TIMEOUT=] [default: 30s]
```

It defaults to 30 seconds.

By [@o0Ignition0o](https://github.com/o0Ignition0o) in https://github.com/apollographql/router/pull/2271

## 🐛 Fixes

### Replace notify recommended watcher with PollWatcher ([Issue #2245](https://github.com/apollographql/router/issues/2245))

We noticed that we kept receiving issues about hot reload. We tried to fix this a while back by moving from HotWatch to Notify, but there are still issues. The problem appears to be caused by having different mechanisms on different platforms. Switching to the PollWatcher, which offers less sophisticated functionality but is the same on all platforms, should solve these issues at the expense of slightly worse reactiveness.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/2276

## 🛠 Maintenance

### Return more consistent errors ([Issue #2101](https://github.com/apollographql/router/issues/2101))

Change some of our errors we returned by following [this specs](https://www.apollographql.com/docs/apollo-server/data/errors/). It adds a `code` field in `extensions` describing the current error. 

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/2178
