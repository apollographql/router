### fix: ensure metrics are recorded for coprocessors that timeout ([PR #9296](https://github.com/apollographql/router/pull/9296))

[jira/ROUTER-1515](https://apollographql.atlassian.net/browse/ROUTER-1515)

This PR introduces two timing histogram macros, `f64_histogram_timer` and `f64_histogram_timer_with_unit`. Although it is deprecated, in this PR `f64_histogram_timer` is used to record the coprocessor timer because introducing a unit for our existing metrics will change the naming convention on services like Prometheus, which we need to avoid. Future metrics should only use `f64_histogram_timer_with_unit`.

This PR also replaces the method of timing coprocessor runs by putting the timer guard in the same scope as the coprocessor run, which ensures that even if the coprocessor is shut down early by a router timeout, the guard will record the metric when the coprocessor goes out of scope. 

<!-- [ROUTER-1515] -->
---

**Checklist**

Complete the checklist (and note appropriate exceptions) before the PR is marked ready-for-review.

- [x] PR description explains the motivation for the change and relevant context for reviewing
- [x] PR description links appropriate GitHub/Jira tickets (creating when necessary)
- [x] Changeset is included for user-facing changes
- [x] Changes are compatible[^1]
- [ ] Documentation[^2] completed
- [ ] Performance impact assessed and acceptable
- [ ] Metrics and logs are added[^3] and documented
- Tests added and passing[^4]
    - [ ] Unit tests
    - [x] Integration tests
    - [ ] Manual tests, as necessary

**Exceptions**

*Note any exceptions here*

**Notes**

[^1]: It may be appropriate to bring upcoming changes to the attention of other (impacted) groups. Please endeavour to do this before seeking PR approval. The mechanism for doing this will vary considerably, so use your judgement as to how and when to do this.
[^2]: Configuration is an important part of many changes. Where applicable please try to document configuration examples.
[^3]: A lot of (if not most) features benefit from built-in observability and `debug`-level logs. Please read [this guidance](https://github.com/apollographql/router/blob/dev/dev-docs/metrics.md#adding-new-metrics) on metrics best-practices.
[^4]: Tick whichever testing boxes are applicable. If you are adding Manual Tests, please document the manual testing (extensively) in the Exceptions.


[ROUTER-1515]: https://apollographql.atlassian.net/browse/ROUTER-1515?atlOrigin=eyJpIjoiNWRkNTljNzYxNjVmNDY3MDlhMDU5Y2ZhYzA5YTRkZjUiLCJwIjoiZ2l0aHViLWNvbS1KU1cifQ

By [@conwuegb](https://github.com/conwuegb) and [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/9296
