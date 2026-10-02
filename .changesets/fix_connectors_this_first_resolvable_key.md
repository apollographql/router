### Fix router startup failure when a connector reads `$this` through `->first`

A field-level `@connect` on an entity that read a `$this` value through `->first` (for example `queryParams: "id: $this.tagIds->first"`) made the router refuse to start with `An internal error has occurred, please report this bug to Apollo. Details: error creating resolvable key`.

The cause was in how connectors work out which `$this` fields an entity key needs. `->first` was recorded as a read of list index `0`, so the synthesized key came out as `tagIds { "0" }`, which is not a valid field set. List indexes are no longer recorded, so the key is now `tagIds`, the same as for `->last` or a plain `$this.tagIds`.

When a connector's key still can't be built, the error now names the connector and includes the field set errors, instead of only saying `error creating resolvable key`.

By [@benjamn](https://github.com/benjamn) in https://github.com/apollographql/router/pull/PULL_NUMBER
