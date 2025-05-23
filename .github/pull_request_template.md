*Description here*

Fixes #**issue_number**

<!-- start metadata -->
---

**Checklist**

Complete the checklist (and note appropriate exceptions) before the PR is marked ready-for-review.

- [ ] Changes are compatible[^1]
- [ ] Documentation[^2] completed
- [ ] Performance impact assessed and acceptable
- [ ] Metrics and logs are added[^3] and documented
- Tests added and passing[^4]
    - [ ] Unit Tests
    - [ ] Integration Tests
    - [ ] Manual Tests

**Exceptions**

*Note any exceptions here*

**Notes**

[^1]: It may be appropriate to bring upcoming changes to the attention of other (impacted) groups. Please endeavour to do this before seeking PR approval. The mechanism for doing this will vary considerably, so use your judgement as to how and when to do this.
[^2]: Configuration is an important part of many changes. Where applicable please try to document configuration examples.
[^3]: A lot of (if not most) features benefit from built-in observability and `debug`-level logs. Please read [this guidance](https://github.com/apollographql/router/blob/dev/dev-docs/metrics.md#adding-new-metrics) on metrics best practices.
[^4]: Tick whichever testing boxes are applicable. If you are adding Manual Tests, please document the manual testing (extensively) in the Exceptions.
