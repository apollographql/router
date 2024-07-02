### Improve `xtask` release process ([PR #5275](https://github.com/apollographql/router/pull/5275))

Introduces a new `xtask` command to automate the release process by:
- Following the commands defined in our `RELEASE_CHECKLIST.md` file
- Storing the current state of the process in the `.release-state.json` file
- Prompting the user regularly for new info.

These changes remove a lot of the manual environment variable setup and command copying previously required.

Executed the new command by running `cargo xtask release start`, then calling `cargo xtask release continue` at each step.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/5275