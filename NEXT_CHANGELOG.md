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

### **Headline** ([Issue #ISSUE_NUMBER](https://github.com/apollographql/router/issues/ISSUE_NUMBER))

Description! And a link to a [reference](http://url)

By [@USERNAME](https://github.com/USERNAME) in https://github.com/apollographql/router/pull/PULL_NUMBER
-->

# [0.11.1] (unreleased) - 2022-mm-dd
## ❗ BREAKING ❗

### Move `experimental.rhai` out of `experimental` [PR #1365](https://github.com/apollographql/router/pull/1365)
You will need to update your YAML configuration file to use the correct name for `rhai` plugin.

```diff
- plugins:
-   experimental.rhai:
-     filename: /path/to/myfile.rhai
+ rhai:
+   scripts: /path/to/directory/containing/all/my/rhai/scripts (./scripts by default)
+   main: <name of main script to execute> (main.rhai by default)
```
You can now modularise your rhai code. Rather than specifying a path to a filename containing your rhai code, the rhai plugin will now attempt to execute the script specified via `main`. If modules are imported, the rhai plugin will search for those modules in the `scripts` directory. for more details about how rhai makes use of modules, look at [the rhai documentation](https://rhai.rs/book/ref/modules/import.html).

The simplest migration will be to set `scripts` to the directory containing your `myfile.rhai` and to rename your `myfile.rhai` to `main.rhai`.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/1365

## 🚀 Features
## 🐛 Fixes

### The opentelemetry-otlp crate needs a http-client feature [PR #1392](https://github.com/apollographql/router/pull/1392)

The opentelemetry-otlp crate only checks at runtime if a HTTP client was added through
cargo features. We now use reqwest for that.

By [@geal](https://github.com/geal) in https://github.com/apollographql/router/pull/1392

## 🛠 Maintenance

### Dependency updates [PR #1389](https://github.com/apollographql/router/issues/1389) [PR #1395](https://github.com/apollographql/router/issues/1395)

Dependency updates were blocked for some time due to incompatibilities:
- #1389: the router-bridge crate needed a new version of `deno_core` in its workspace that would not fix the version of `once_cell`. Now that it is done we can update `once_cell` in the router
- #1395: `clap` at version 3.2 changed the way values are extracted from matched arguments, which resulted in panics. This is now fixed and we can update `clap` in the router and related crates

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/1389 https://github.com/apollographql/router/pull/1395

### Insert the full target triplet in the package name [PR #1393](https://github.com/apollographql/router/pull/1393)

The released package names will now contain the full target triplet in their name:

* `router-0.11.0-x86_64-linux.tar.gz` -> `router-0.11.0-x86_64-unknown-linux-gnu.tar.gz`
* `router-0.11.0-x86_64-macos.tar.gz` -> `router-0.11.0-x86_64-apple-darwin.tar.gz`
* `router-0.11.0-x86_64-windows.tar.gz` -> `router-0.11.0-x86_64-pc-windows-msvc.tar.gz`

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/1393

## 📚 Documentation
