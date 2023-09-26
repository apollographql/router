### Docker Image: fix the DIY build_docker_image.sh script. ([Issue #3823](https://github.com/apollographql/router/issues/3823))

The DIY `build_docker_image.sh` script was broken by the latest `deno` update due to a new requirement to have `cmake` when compiling the router.

This adds `cmake` to the build step for DIY builds to fix the problem.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/3824
