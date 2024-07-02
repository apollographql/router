### xtask release process ([PR #5275](https://github.com/apollographql/router/pull/5275))

This introduces a new xtask command to autoçmate the release process, by following the commands defined in our `RELEASE_CHECKLIST.md` file, storing the current state of the process in the file `.release-state.json`, and prompting the user regularly for new info. It removes a lot of the manual environment variable setup and command copying that we do regularly.

This can be executed by running `cargo xtask release start`, then calling `cargo xtask release continue` at each step.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/5275