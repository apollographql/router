### Retry `mise install` in CircleCI to absorb transient GitHub release fetch failures

The `install_mise` step in `.circleci/config.yml` ran `mise install` exactly once, so any transient 404 from GitHub releases (mise's aqua backend pulls each pinned tool from `github.com/.../releases/download/...`) failed an otherwise-healthy job. We've seen this surface as three different jobs failing in the same workflow, each on a different tool (kubeconform, protoc, gh) — the signature of intermittent CDN/rate-limit flakes rather than a config bug.

Wrap both invocations (Linux/macOS and Windows) in a 3-attempt loop with linear 5s/10s backoff. On the third failure the step still exits non-zero so genuine configuration errors are not masked.

By [@aaronArinder](https://github.com/aaronArinder) in https://github.com/apollographql/router/pull/9497
