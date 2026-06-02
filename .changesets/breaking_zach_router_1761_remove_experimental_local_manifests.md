### Remove `experimental_local_manifests` persisted queries config alias

The `persisted_queries.experimental_local_manifests` configuration key — a compatibility alias for `persisted_queries.local_manifests` that emitted a deprecation warning throughout 2.x — has been removed. The stable `local_manifests` key has existed since the feature graduated and is the only supported name in 3.x.

If your router configuration still uses the experimental key, rename it:

```diff
 persisted_queries:
   enabled: true
-  experimental_local_manifests:
+  local_manifests:
     - ./persisted-queries-manifest.json
```

Configurations that still reference `experimental_local_manifests` will fail to load with an "unknown field" error on startup.

By [@zachfetters](https://github.com/zachfetters) in https://github.com/apollographql/router/pull/9532
