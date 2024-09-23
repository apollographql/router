### Allow disabling persisted-queries-based query plan cache prewarm on schema reload

The router supports the new `persisted_queries.experimental_prewarm_query_plan_cache.on_reload` configuration option. It toggles whether a query plan cache that's prewarmed upon loading a new schema includes operations from persisted query lists. Its default is `true`. Setting it `false` precludes operations from persisted query lists from being added to the prewarmed query plan cache.

Some background about the development of this option:

- In router v1.31.0, we started including operations from persisted query lists when the router prewarms the query plan cache when loading a new schema.

- Then in router v1.49.0, we let you also prewarm the query plan cache from the persisted query list during router startup by setting `persisted_queries.experimental_prewarm_query_plan_cache` to true.

- In this release, we now allow you to disable the original feature so that the router can prewarm only recent operations from the query planning cache (and not operations from persisted query lists) when loading a new schema.

Note: the option added in v1.49.0 has been renamed from `persisted_queries.experimental_prewarm_query_plan_cache` to `persisted_queries.experimental_prewarm_query_plan_cache.on_startup`. Existing configuration files will keep working as before, but with a warning that can be resolved by updating your config file:

```diff
 persisted_queries:
   enabled: true
-  experimental_prewarm_query_plan_cache: true
+  experimental_prewarm_query_plan_cache:
+    on_startup: true
```


By [@glasser](https://github.com/glasser) in https://github.com/apollographql/router/pull/5990