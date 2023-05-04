### Make test_experimental_notice assertion more targeted ([Pull #3036](https://github.com/apollographql/router/pull/3036))

Previously this test relied on a full snapshot of the log. This is likely to result in failures either due to environmental reasons or other unrelated changes.
The test now relies on a more targeted assertion that is less likely to fail.

By [@bryncooke](https://github.com/bryncooke) in https://github.com/apollographql/router/pull/3036
