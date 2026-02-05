### Bug: Connectors content-type header duplication ([PR #8867](https://github.com/apollographql/router/pull/8867))

When using connectors and overriding the content-type header, the default value was still set. Setting the content-type header will now properly override the value.

For example:

```
@source(
    name: "datasetInsightsAPI"
    http: {
        headers: [
            { name: "Content-Type", value: "application/vnd.iaas.v1+json" },
        ]
    }
)
```

Would result in:

```
content-type: application/json, application/vnd.iaas.v1+json
```

After this change, it will instead result in:

```
content-type: application/vnd.iaas.v1+json
```

By [@andrewmcgivery](https://github.com/andrewmcgivery) in https://github.com/apollographql/router/pull/8867
