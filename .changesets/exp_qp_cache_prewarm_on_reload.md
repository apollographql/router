### Allow disabling persisted-queries-based query plan cache prewarm on schema reload

In Router v1.31.0, we started including operations from persisted query lists when Router pre-warms the query plan cache when loading a new schema.

In Router v1.49.0, we let you also pre-warm the query plan cache from the persisted query list during Router startup by setting `persisted_queries.experimental_prewarm_query_plan_cache` to true.

We now allow you to disable the original feature, so that Router will only pre-warm recent operations from the query planning cache when loading a new schema (and not the persisted query list as well), by setting `persisted_queries.experimental_prewarm_query_plan_cache.on_reload` to `false`.

The option added in v1.49.0 has been renamed from `persisted_queries.experimental_prewarm_query_plan_cache` to `persisted_queries.experimental_prewarm_query_plan_cache.on_startup`. Existing configuration files will keep working as before, but with a warning that can be resolved by updating your config file:

```diff
 persisted_queries:
   enabled: true
-  experimental_prewarm_query_plan_cache: true
+  experimental_prewarm_query_plan_cache:
+    on_startup: true
```


By [@glasser](https://github.com/glasser) in https://github.com/apollographql/router/pull/5990