### Tidy up various thread interactions ([Issue #4121](https://github.com/apollographql/router/issues/4121))

Introduce the DropWatch utility struct which can be used to watch a closure and either:
 - execute the closure and then wait for the DropWatch struct to be dropped
 - or wait for the struct to be dropped and then execute the closure

It's primarily useful when dealing with Drops that are triggered from an async context, but could be useful wherever we have a thread that we'd like to have a managed lifetime associated with a Drop.

We use it to clean up thread interactions in Telemetry and Rhai file watching.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/4127