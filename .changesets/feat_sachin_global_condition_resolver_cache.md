### Reuse condition planning cache during query planning and satisfiability validation ([PR #9740](https://github.com/apollographql/router/pull/9740))

Router query planning and composition satisfiability validation have optimizations that cache the results of planning a "condition" (e.g. the fields of an `@key` or `@requires`). However, when this condition planning happened recursively, this resulted in new caches being constructed instead of reusing the existing cache. This behavior inhibited the reuse of previous condition planning work, increasing planning/validation time significantly in some circumstances. This code has now been changed to reuse the existing cache.

By [@sachindshinde](https://github.com/sachindshinde) in https://github.com/apollographql/router/pull/9740