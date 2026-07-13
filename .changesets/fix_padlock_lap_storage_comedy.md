### Update operation validation to enforce unique `@defer` labels

Adds a defensive check in the query planner to detect duplicate `@defer` labels. The operation validation will now ensure that we have unique labels.

By [@duckki](https://github.com/duckki)