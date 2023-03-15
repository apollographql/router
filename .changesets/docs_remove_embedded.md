### Remove embedded example ([Issue #2737](https://github.com/apollographql/router/issues/2737))

The embedded example is a throwback to early days of the Router where distribution as middleware was considered viable.
As development has progressed this approach has become obsolete as we have baked some of our functionality into the webserver layer.
In addition, the entire example uses the `TestHarness` which is designed for testing rather than production traffic.

By [@bryncooke](https://github.com/bryncooke) in https://github.com/apollographql/router/pull/2738
