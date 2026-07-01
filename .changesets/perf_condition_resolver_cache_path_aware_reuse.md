### Reuse cached satisfied condition resolutions when the path avoids newly-excluded destinations ([PR #9742](https://github.com/apollographql/router/pull/9742))

Adds path-aware reuse to the condition resolver cache. When a cached
`Satisfied` resolution exists for an edge, and the resolution's path
tree does not traverse any of the newly-excluded destinations, the
cached result is reused instead of re-resolving the condition from
scratch.

This is safe because the path tree records which subgraphs were used
to satisfy the condition. If none of those subgraphs appear in the
new exclusions, the path remains available and the resolution is still
valid.

By [@tninesling](https://github.com/tninesling)
