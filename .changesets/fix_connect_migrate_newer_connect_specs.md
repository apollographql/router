### `connect-migrate analyze` no longer mis-analyzes schemas on newer `connect` spec versions ([PR #10268](https://github.com/apollographql/router/pull/10268))

`connect-migrate analyze` recognized `connect/v0.1` through `connect/v0.4` and quietly treated anything else as `connect/v0.3`. A subgraph linking `connect/v0.5` was therefore diffed as if its selections had been written against the v0.3 grammar, and the manifest reported `safe-after-rewrites` with deterministic `$.` fortifications to apply. Those edits would have changed the behavior of a schema that needed no migration at all, and the manifest closed by telling the user to move its `@link` back to `connect/v0.4`.

Three changes:

- `connect/v0.5` is recognized. It shares v0.4's selection grammar, so a v0.5 schema diffs clean and is reported as already at the target.
- A schema linking a `connect` version the binary does not know is no longer analyzed against a guessed baseline. It is skipped and listed under a new `## Heads up — schemas not analyzed` section, counted by a `schemas-skipped:` header field. The manifest says plainly that its verdict does not cover those schemas.
- Manifests for a project already on `connect/v0.4` or newer carry `already-at-target: true` and drop the advice to update the `@link`, which for a newer schema was advice to downgrade.

The embedded agent guide (`connect-migrate agent-guide`) is also resynced with [`SKILL.md`](https://github.com/apollographql/connect-migrate/blob/main/SKILL.md), from which it is derived. It had drifted: it still described the `connect-migrate` repository as private and its installer as unusable without authentication, both of which stopped being true when the repository was made public. The guide now also says `connect/v0.4` is the migration target: agents should not propose `connect/v0.5`, which is still a preview spec, and should warn before following a developer's explicit request for it.

By [@benjamn](https://github.com/benjamn) in https://github.com/apollographql/router/pull/10268
