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

# [0.14.1] (unreleased) - 2022-mm-dd

## ❗ BREAKING ❗

### Reference-counting for the schema string given to plugins ([PR #???](https://github.com/apollographql/router/pull/))

The type of the `supergraph_sdl` field of the `apollo_router::plugin::PluginInit` struct
was changed from `String` to `Arc<String>`.
This reduces the number of copies of this string we keep in memory, as schemas can get large.

By [@SimonSapin](https://github.com/SimonSapin)

### Changes to `PluginTestHarness` API ([PR #1468](https://github.com/apollographql/router/pull/1468))

The `plugin` method of `PluginTestHarness`’s builder was removed.
Users of `PluginTestHarness` don’t create plugin instances themselves anymore.

Instead, the builder has a new mandatory `configuration` method,
which takes the full Router configuration as would be found in a `router.yaml` file.
Through that configuration, plugins can be enabled (and configured) by name.
Instead of YAML syntax though, the method takes a `serde_json::Value`.
A convenient way to create such a value in Rust code is with the `json!` macro.

The `IntoSchema` enum has been removed.
The `schema` method of the builder is now optional and takes a `&str`.
If not provided, the canned testing schema is used by default.

Changes to tests for an example plugin:

```diff
-use apollo_router::plugin::test::IntoSchema::Canned;
 use apollo_router::plugin::test::PluginTestHarness;
-use apollo_router::plugin::Plugin;
-use apollo_router::plugin::PluginInit;
+use serde_json::json;
 
-let conf = MyPluginConfig {
-    something: "something".to_string(),
-};
-let plugin = MyPlugin::new(PluginInit::new(conf, Default::default()))
-    .await
-    .unwrap();
+let conf = json!({
+    "plugins": {
+        "example.my_plugin": {
+            "something": "something"
+        }
+    }
+});
 let test_harness = PluginTestHarness::builder()
-            .plugin(plugin)
-            .schema(Canned)
+            .configuration(conf)
             .build()
             .await
             .unwarp();
```

By [@SimonSapin](https://github.com/SimonSapin)

## 🚀 Features

## 🐛 Fixes

### Configuration handling enhancements ([PR #1454](https://github.com/apollographql/router/pull/1454))

Router config handling now:
* Allows completely empty configuration without error.
* Prevents unknown tags at the root of the configuration from being silently ignored.

By [@bryncooke](https://github.com/bryncooke) in https://github.com/apollographql/router/pull/1454


## 🛠 Maintenance

## 📚 Documentation

### Add helm OCI example ([PR #1457](https://github.com/apollographql/router/pull/1457))

Update existing filesystem based example to illustrate how to do the same thing using our OCI stored helm chart.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/1457
