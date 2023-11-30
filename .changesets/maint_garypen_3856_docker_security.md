### Improve the secure deployability of our Helm Chart and Docker Image ([Issue #3856](https://github.com/apollographql/router/issues/3856))

This is a security improvement for the Apollo Router that is achieved by:
 - Switching the router process owner from `root` to a user with less privileges
 - Changing the default port from 80 to 4000
 - Updating the base image from bullseye (Debian 11) to bookworm (Debian 12)

The primary motivations for these changes is that many Kubernetes environments impose security restrictions on containers. For example:
 - Don't run as root
 - Can't bind to ports < 1024

With these changes in place, the router is more secure by default and much simpler to deploy to secure environments.

The base Debian image has also been updated at this time to keep track with bug fixes in the base image.

Changing the default port in the Helm chart from 80 to 4000 is an innocuous change. This shouldn't impact most users. Changing the default user from `root` to `router` will have an impact. You will no longer be able to `exec` to the executing container (Kubernetes or Docker) and perform root privilege operations. The container is now "locked down", by default. Good for security, but less convenient for support or debugging.

Although it's not recommended to revert to the previous behavior of the router executing as root and listening on port 80, it's possible to achieve that with the following configuration:

```
router:
  configuration:
    supergraph:
      listen: 0.0.0.0:80
securityContext:
  runAsUser: 0
```

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/3971
