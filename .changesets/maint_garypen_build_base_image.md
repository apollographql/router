### On linux the router is now compatible with GLIBC 2.28 or newer ([PR #7355](https://github.com/apollographql/router/pull/7355))

The default build images provided in our CI environment have a relatively modern version of GLIBC (2.35). This means that on some distributions, notably those based around RedHat, it wasn't possible to use our binaries since the version of GLIBC was older than 2.35.

We now maintain a build image which is based on a distribution with GLIBC version of 2.28. This is old enough that recent releases of either of the main linux distribution familes (Debian and RedHat) can make use of our binary releases.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/7355