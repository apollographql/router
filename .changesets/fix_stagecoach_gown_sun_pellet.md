### Config and schema reloads now use async IO ([Issue #2613](https://github.com/apollographql/router/issues/2613))

If you were using local schema or config then previously the Router was performing blocking IO in an async thread. This could have caused stalls to serving requests and was generally bad practice.
The Router now uses async IO for all config and schema reloads.

Fixing the above surfaced an issue with the experimental `force_hot_reload` feature introduced for testing. This has also been fixed and renamed to `force_reload`. 

```diff
experimental_chaos:
-    force_hot_reload: 1m
+    force_reload: 1m
```

By [@bryncooke](https://github.com/bryncooke) in https://github.com/apollographql/router/pull/3016
