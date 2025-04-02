### Enable configuation auto migration for minor version bumps ([PR #7162](https://github.com/apollographql/router/pull/7162))

Enable configuration auto migration when running the Router, only when it's migration written for the related version.
Example: if you're running on Router 2.x, only migrations named `2xxx_*.yaml` will be automatically applied. Previous migrations will have to be applied using `router upgrade` command. 

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/7162