### Enable Integer Error Code Reporting ([PR #7226](https://github.com/apollographql/router/pull/7226))

Fixes an issue where numeric error codes (e.g. 400, 500) were not properly parsed into a string and thus were not
reported to Apollo error telemetry.

By [@rregitsky](https://github.com/rregitsky) in https://github.com/apollographql/router/pull/7226
