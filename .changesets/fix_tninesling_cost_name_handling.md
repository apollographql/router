### Ensure cost directives are picked up when not explicitly imported ([PR #6328](https://github.com/apollographql/router/pull/6328))

With the recent composition changes, importing `@cost` results in a supergraph schema with the cost specification import at the top. The `@cost` directive itself is not explicitly imported, as it's expected to be available as the default export from the cost link. In contrast, uses of `@listSize` to translate to an explicit import in the supergraph.

Old SDL link

```
@link(
    url: "https://specs.apollo.dev/cost/v0.1"
    import: ["@cost", "@listSize"]
)
```

New SDL link

```
@link(url: "https://specs.apollo.dev/cost/v0.1", import: ["@listSize"])
```

Instead of using the directive names from the import list in the link, the directive names now come from `SpecDefinition::directive_name_in_schema`, which is equivalent to the change we made on the composition side.

By [@tninesling](https://github.com/tninesling) in https://github.com/apollographql/router/pull/6328
