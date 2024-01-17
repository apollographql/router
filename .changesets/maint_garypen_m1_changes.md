### Pre-built binaries are only available for Apple Silicon ([Issue #3902](https://github.com/apollographql/router/issues/3902))

Prior to this release, macOS binaries were produced on Intel build machines in our CI pipeline. The binaries produced would also work on Apple Silicon (M1, etc.. chips) through the functionality provided by [Rosetta2](https://support.apple.com/en-gb/HT211861).

Our [CI provider has announced the deprecation of the macOS Intel build machines](https://discuss.circleci.com/t/macos-intel-support-deprecation-in-january-2024/48718) and we are updating our build pipeline to use the new Apple Silicon based machines.

This will have the following effects:
 - Older, Intel based, macOS systems will no longer be able to execute our macOS router binaries
 - Newer, Apple Silicon based, macOS systems will get a performance boost due to no longer requiring Rosetta2 support.

We have raised [an issue](https://github.com/apollographql/router/issues/4483) that describes options for Intel based macOS users. Please let us know in that issue if the alternatives we suggest (e.g., Docker, source build) don't work for you so we can discuss alternatives.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/4484