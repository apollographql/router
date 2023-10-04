### Improve the secure deployability of our helm chart and docker image ([Issue #3856](https://github.com/apollographql/router/issues/3856))

Many kubernetes environments impose security restrictions on containers. For example:
 - Don't run as root
 - Can't bind to ports < 1024

Right now our container runs the router as `root` on port 80 and this makes it harder to deploy in such secure environments.

This change moves our helm chart's default port from 80 to 4000 and introduces a new user `router` in our docker container which we will use to run our router process.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/3971