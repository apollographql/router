### Remove unneeded schema parsing steps ([PR #3547](https://github.com/apollographql/router/pull/3547))

We need access to a parsed schema in various parts of the router, sometimes before the point where it is actually parsed and integrated with the rest of the configuration, so it was parsed multiple times to mitigate that. Some architecture changes made these parsing steps obsolete so they were removed.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/3547