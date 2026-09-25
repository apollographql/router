### Check current fetch ancestry during optimization

Recheck ancestry before combining equal-input fetches. Earlier combinations can invalidate the initial topological ordering; keeping a now-unsafe pair separate prevents an attempted cycle and preserves its selected work.

By [@inanna-apollo](https://github.com/inanna-apollo) in https://github.com/apollographql/router/pull/10291
