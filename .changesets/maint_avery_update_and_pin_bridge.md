### chore: router-bridge 0.3.0+v2.4.8 -> =0.3.1+2.4.9 ([PR #3407](https://github.com/apollographql/router/pull/3407))

Updates `router-bridge` from ` = "0.3.0+v2.4.8"` to ` = "0.3.1+v2.4.9"`, note that with this PR, this dependency is now pinned to an exact version. This version update started failing tests because of a minor ordering change and it was not immediately clear why the test was failing. Pinning this dependency (that we own) allows us to only bring in the update at the proper time and will make test failures caused by the update to be more easily identified.

By [@EverlastingBugstopper](https://github.com/EverlastingBugstopper) in https://github.com/apollographql/router/pull/3407
