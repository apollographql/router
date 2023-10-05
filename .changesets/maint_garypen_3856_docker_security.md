### Improve the secure deployability of our helm chart and docker image ([Issue #3856](https://github.com/apollographql/router/issues/3856))

Many kubernetes environments impose security restrictions on containers. For example:
 - Don't run as root
 - Can't bind to ports < 1024

Right now our container runs the router as `root` on port 80 and this makes it harder to deploy in such secure environments.

This change moves our helm chart's default port from 80 to 4000 and introduces a new user `router` in our docker container which we will use to run our router process.

**Impact**

1. Changing the default port in the helm chart from 80 to 4000 is an innocuous change. This shouldn't impact anyone.
2. Changing the default user from `root` to `router` will have an impact. You will no longer be able to `exec` to the executing container (kubernetes or docker) and install packages, etc... The container is now "locked down", by default. Good for security, but less convenient for support/debugging.

If you wish to restore the behaviour of the router to execute as root and listen on port 80, that can be achieved with this configuration:

```
router:
  configuration:
    supergraph:
      listen: 0.0.0.0:80
securityContext:
  runAsUser: 0
```
We don't recommend that you do this for debugging/support (there are more secure ways to achieve your goals in kubernetes), but we are documenting for completeness.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/3971
