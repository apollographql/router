### Preserve ancestor conditions across entity fetches

The incremental query planner dropped ancestor `@skip` and `@include` conditions at entity fetch boundaries. A mutation field under a skipped fragment could still execute when its subgraph fetch also selected `@requires`, `@fromContext`, or key inputs for a later fetch. Entity fetches and the inputs they read now stay under their path conditions. The planner also keeps conditions shared by alternative selections and drops fetches whose ancestor and descendant conditions contradict.

By [@tninesling](https://github.com/tninesling)
