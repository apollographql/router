### fix the diy build_docker_image.sh script ([Issue #3823](https://github.com/apollographql/router/issues/3823))

The diy `build_docker_image.sh` script was broken with the latest `deno` update due to a new requiremetn to have `cmake` when compiling the router.

This adds `cmake` to the build step for diy builds.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/3824