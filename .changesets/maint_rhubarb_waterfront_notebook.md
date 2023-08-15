### Add a message to the logs indicating when custom plugins are detected and there is a possibility that log entries may be silenced ([Issue #3526](https://github.com/apollographql/router/issues/3526))

Since [#3477](https://github.com/apollographql/router/pull/3477), users who have created custom plugins no longer see their log entries.
This is because the default logging filter now restricts log entries to those that are in the apollo module.

Users that have custom plugins will need to configure the logging filter to include their modules, but they may not realise this.

Now, if a custom plugin is detected then a message will be logged to the console indicating that the logging filter may need to be configured.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/3540
