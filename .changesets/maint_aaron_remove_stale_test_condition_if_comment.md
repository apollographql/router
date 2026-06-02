### Remove stale "NOT migrated" comment above `test_condition_if` ([Issue/PR #9497](https://github.com/apollographql/router/pull/9497))

Earlier in PR #9497, commit `ea97375ea` left a block comment above `test_condition_if` explaining that the test was deferred from the `get_trace_report` migration because of a snapshot ordering inconsistency. Commit `eb84b6642` then completed the migration *and* re-blessed both `apollo_reports__condition_if.snap` and `apollo_reports__condition_if-2.snap` — but missed deleting the now-stale comment. Surfaced by ultrareview on PR #9497.

By [@aaronArinder](https://github.com/aaronArinder) in https://github.com/apollographql/router/pull/9497
