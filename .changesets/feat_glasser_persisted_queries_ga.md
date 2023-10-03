### Move Persisted Queries to General Availability ([PR #3914](https://github.com/apollographql/router/pull/3914))

[Persisted Queries](https://www.apollographql.com/docs/graphos/operations/persisted-queries/) (a GraphOS Enterprise feature) is now moving to General Availability, from Preview where it has been since Apollo Router 1.25.

For more information about launch stages, please see the documentation here: https://www.apollographql.com/docs/resources/product-launch-stages/

The feature is now configured with a `persisted_queries` top-level key in the YAML configuration instead of with `preview_persisted_queries`. Existing configuration files will keep working as before, only with a warning. To fix that warning, rename the configuration section like so:

```diff
-preview_persisted_queries:
+persisted_queries:
   enabled: true
```

By [@glasser](https://github.com/glasser) in https://github.com/apollographql/router/pull/3914