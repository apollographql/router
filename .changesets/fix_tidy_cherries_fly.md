### Some response objects gets incorrectly set to `null` in some cases introduced by `@interfaceObject`

The federation 2.3 `@interfaceObject` feature imply that an interface type in the supergraph may be locally handled as an object type by some specific subgraphs. Such subgraph may thus return objects whose `__typename` is the interface type in their response. In some cases, those `__typename` were leading the router to unexpectedly nullify the underlying objects and this was not caught in the initial integration of federation 2.3.

By [@pcmanus](https://github.com/pcmanus) in https://github.com/apollographql/router/pull/2530
