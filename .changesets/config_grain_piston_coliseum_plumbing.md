### deprecate `traffic_shaping.deduplicate_variables` config field ([PR #9586](https://github.com/apollographql/router/pull/9586))

[jira/ROUTER-1768](https://apollographql.atlassian.net/browse/ROUTER-1768)

<!-- [ROUTER-1768] -->

Add a startup deprecation warning when `traffic_shaping.deduplicate_variables` is set which alerts operators to remove the field from their config. Since variable deduplication is unconditionally enabled, the field is silently ignored and will be removed.

[ROUTER-1768]: https://apollographql.atlassian.net/browse/ROUTER-1768?atlOrigin=eyJpIjoiNWRkNTljNzYxNjVmNDY3MDlhMDU5Y2ZhYzA5YTRkZjUiLCJwIjoiZ2l0aHViLWNvbS1KU1cifQ

By [@conwuegb](https://github.com/conwuegb) in https://github.com/apollographql/router/pull/9586
