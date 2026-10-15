### Fix `CONNECTORS_CANNOT_RESOLVE_KEY` and router startup failure when a connector reads `$this` through `->first`

A field-level `@connect` on an entity that read a `$this` value through `->first` (for example `queryParams: "id: $this.tagIds->first"`) failed composition with `CONNECTORS_CANNOT_RESOLVE_KEY`. With a supergraph composed by an older version, the router refused to start with `An internal error has occurred, please report this bug to Apollo. Details: error creating resolvable key`.

The cause was in how connectors work out which `$this` fields an entity key needs. `->first` was recorded as a read of list index `0`, so the synthesized key came out as `tagIds { "0" }`, which is not a valid field set. List indexes are no longer recorded, so the key is now `tagIds`, the same as for `->last` or a plain `$this.tagIds`. Schemas like this now compose and the router starts.

Reading fields from the first element of a list of objects, such as `$this.items->first.id` or `$this.items->first { id }`, also composes now, with the key `items { id }`. This never composed in earlier versions. A bare `$this.items->first` on a list of objects is still rejected with `CONNECTORS_CANNOT_RESOLVE_KEY`, the same as a bare `$this.items`, because the key needs to select fields from the objects.

When a connector's key still can't be built, the error now names the connector and includes the field set errors, instead of only saying `error creating resolvable key`.

By [@benjamn](https://github.com/benjamn) in https://github.com/apollographql/router/pull/10403
