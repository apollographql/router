
# Strawdog proposal
Goal was to try and combine the approaches that have been submitted so far.

## Components can be overridden
Use abstract factory pattern to allow users to override some or all components:
* [Factory trait](https://github.com/apollographql/bryn/blob/extensions-strawdog/src/main.rs#L168)
* [Custom implementation](https://github.com/apollographql/bryn/blob/extensions-strawdog/src/custom_orchestration.rs#L28)

## Router may be embedded
[Surface a handler so that the router may be embedded](https://github.com/apollographql/bryn/blob/extensions-strawdog/src/main.rs#L219)
Warp/Other connectors can be developed.

## Use as a binary or as a library
Via a [default implementation](https://github.com/apollographql/bryn/blob/extensions-strawdog/src/main.rs#L212) that uses warp.

## An extension API is created
An [Extension trait](https://github.com/apollographql/bryn/blob/extensions-strawdog/src/main.rs#L128) for use internally at first, but eventually for users.

Chain of responsibility pattern is used to preserve stack, reduce the need to store context variables, allow 
retry/blocking/timeouts in a natural way. For example:
* [Header propagation](https://github.com/apollographql/bryn/blob/extensions-strawdog/src/extensions.rs#L16)
* [Security](https://github.com/apollographql/bryn/blob/extensions-strawdog/src/extensions.rs#L37)
* [Retry](https://github.com/apollographql/bryn/blob/extensions-strawdog/src/extensions.rs#L49)
* [OtelExtension](https://github.com/apollographql/bryn/blob/extensions-strawdog/src/extensions.rs#L66)

In addition, we can create a [WasmExtension](https://github.com/apollographql/bryn/blob/extensions-strawdog/src/extensions.rs#L78)
from this model.

## Components and extensions are able to configure themselves
Configuration is delegated to implementations, which will use serde internally on a subpath of the config.
* [Extensions](https://github.com/apollographql/bryn/blob/extensions-strawdog/src/main.rs#L129)
* [Components](https://github.com/apollographql/bryn/blob/extensions-strawdog/src/main.rs#L169-L188)


