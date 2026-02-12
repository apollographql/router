### Add config to opt-out of stricter variable validation ([PR #8884](https://github.com/apollographql/router/pull/8884))

Variable validation will become **_stricter by default_** due to [PR#8821](https://github.com/apollographql/router/pull/8821). 
This PR fixed a gap in variable validation whereby the presence of unknown fields on an input object variable were not causing a request error as they should have.

This stricter validation **_may cause breakages_** for customers. 

To alleviate that potential pain point while customers update their variables to be compliant, this change introduces a router config option to retain the previous level of validation and issue a warning log instead of an error.

> [!WARNING]
> If you need to opt out, you must set the config option to `warn` instead.

Enabled:
```yaml
supergraph:
  strict_variable_validation: enforce
```

Disabled:
```yaml
supergraph:
  strict_variable_validation: warn
```

Docs have also been updated to reflect this change.

<!-- [ROUTER-1602] -->
---

[ROUTER-1602]: https://apollographql.atlassian.net/browse/ROUTER-1602?atlOrigin=eyJpIjoiNWRkNTljNzYxNjVmNDY3MDlhMDU5Y2ZhYzA5YTRkZjUiLCJwIjoiZ2l0aHViLWNvbS1KU1cifQ

By [@carodewig](https://github.com/carodewig) and [@conwuegb](https://github.com/conwuegb) in https://github.com/apollographql/router/pull/8884
