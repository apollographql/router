### Move Persisted Queries to General Availability ([PR #3914](https://github.com/apollographql/router/pull/3914))

[Persisted Queries](https://www.apollographql.com/docs/graphos/operations/persisted-queries/) (a GraphOS Enterprise feature) is now moving to General Availability, from Preview where it has been since Apollo Router 1.25. In addition to Safelisting, persisted queries can now also be used to [pre-warm the query plan cache](https://github.com/apollographql/router/releases/tag/v1.31.0) to speed up schema updates. 


The feature is now configured with a `persisted_queries` top-level key in the YAML configuration instead of with `preview_persisted_queries`. Existing configuration files will keep working as before, only with a warning. To fix that warning, rename the configuration section like so:

```diff
-preview_persisted_queries:
+persisted_queries:
   enabled: true
```

By [@glasser](https://github.com/glasser) in https://github.com/apollographql/router/pull/3914