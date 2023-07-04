### Move operation limits and parser limits to General Availability ([PR #3356](https://github.com/apollographql/router/pull/3356))

[Operation Limits](https://www.apollographql.com/docs/router/configuration/operation-limits) (a GraphOS Enterprise feature) and [parser limits](https://www.apollographql.com/docs/router/configuration/overview/#parser-based-limits) are now moving to General Availability, from Preview where they have been since Apollo Router 1.17.

For more information about launch stages, please see the documentation here: https://www.apollographql.com/docs/resources/product-launch-stages/

In addition to removing the `preview_` prefix, the configuration section has been renamed to just `limits` to encapsulate operation, parser and request limits. ([The request size limit](https://www.apollographql.com/docs/router/configuration/overview/#request-limits) is still [experimental](https://github.com/apollographql/router/discussions/3220).) Existing configuration files will keep working as before, only with a warning. To fix that warning, rename the configuration section like so:


```diff
-preview_operation_limits:
+limits:
   max_depth: 100
   max_height: 200
   max_aliases: 30
   max_root_fields: 20
```

By [@SimonSapin](https://github.com/SimonSapin) in https://github.com/apollographql/router/pull/3356
