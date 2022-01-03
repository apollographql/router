
# Strawdog proposal
Goal was to try and combine the approaches that have been submitted so far.

## Components can be overridden
Use abstract factory pattern to allow users to override some or all components:
* Factory trait: https://github.com/apollographql/bryn/blob/extensions-strawdog/src/main.rs#L169

* Custom implementation: https://github.com/apollographql/bryn/blob/extensions-strawdog/src/custom_orchestration.rs#L28

## Router may be embedded
Surface a handler so that the router may be embedded:
https://github.com/apollographql/bryn/blob/extensions-strawdog/src/main.rs#L220

## We are still provide a default implementation that users will use either as a binary or as a library
https://github.com/apollographql/bryn/blob/extensions-strawdog/src/main.rs#L213

## An extension API is created
For use internally at first, but eventually for users:
https://github.com/apollographql/bryn/blob/extensions-strawdog/src/main.rs#L129

Chain of responsibility pattern is used to preserve stack, reduce the need to store context variables, allow 
retry/blocking/timeouts in a natural way:
https://github.com/apollographql/bryn/blob/extensions-strawdog/src/extensions.rs#L17

A WASM extension can be created:
https://github.com/apollographql/bryn/blob/extensions-strawdog/src/extensions.rs#L78

## Components and extensions are able to configure themselves
Configuration is delegated to implementations, which will use serde internally on a subpath of the config.
https://github.com/apollographql/bryn/blob/extensions-strawdog/src/main.rs#L130
https://github.com/apollographql/bryn/blob/extensions-strawdog/src/main.rs#L170-L189


