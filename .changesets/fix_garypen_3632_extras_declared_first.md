### Declare `extraContainers` ahead of the router container ([Issue #3632](https://github.com/apollographql/router/issues/3632))

Currently `extraContainers` are declared after the router container. Moving the `extraContainers` ahead of the router container will make it simpler to co-ordinate container startup sequencing and take full advantage of kubernetes lifecycle hooks.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/3633