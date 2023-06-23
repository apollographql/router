### Add additional build functionality to the diy build script ([Issue #3303](https://github.com/apollographql/router/issues/3303))

The diy build script is useful for ad-hoc image creation during testing or for building your own images based on a router repo. This set of enhancements makes it possible to

 - build docker images from arbitrary (nightly) builds (-a)
 - build an amd64 docker image on an arm64 machine (or vice versa) (-m)
 - change the name of the image from the default 'router' (-n)

Note: the build machine image architecture is used if the -m flag is not supplied.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/3304