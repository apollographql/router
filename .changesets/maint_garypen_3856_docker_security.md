### Improve the secure deployability of our Helm Chart and Docker Image ([Issue #3856](https://github.com/apollographql/router/issues/3856))

This is a security improvement achieved by:
 - switching the router process owner from root to a user with less privileges
 - moving the default port from 80 to 4000
 - updating our base image from bullseye (Debian 11) to bookworm (Debian 12)

The primary motivations for these changes is that many Kubernetes environments impose security restrictions on containers. For example:
 - Don't run as root
 - Can't bind to ports < 1024

With these changes in place, the router is more secure by default and much simpler to deploy to secure environments.

We are also choosing to update our base Debian image at this time to keep track with bug fixes in the base image.

Changing the default port in the helm chart from 80 to 4000 is an innocuous change. This shouldn't impact most users. Changing the default user from `root` to `router` will have an impact. You will no longer be able to `exec` to the executing container (Kubernetes or Docker) and perform root privilege operations. The container is now "locked down", by default. Good for security, but less convenient for support/debugging.

If you wish to restore the old behaviour of the router to execute as root and listen on port 80, that can be achieved with this configuration:

```
router:
  configuration:
    supergraph:
      listen: 0.0.0.0:80
securityContext:
  runAsUser: 0
```

We don't recommend that you do this for debugging/support (there are more secure ways to achieve your goals in Kubernetes), but we are documenting for completeness.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/3971
