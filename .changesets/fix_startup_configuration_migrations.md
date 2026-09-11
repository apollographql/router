### Configuration that still fails validation after a startup migration no longer runs

When a startup migration changed a configuration file but the result still failed validation, the router previously fell back to running on the original, un-migrated file. It now reports the validation error instead, so the router never starts on configuration you were already warned to stop using.

When a startup migration changes the configuration, the router now also warns that any validation error line numbers and snippets refer to the migrated document, not the file on disk. Running `router config upgrade` and saving its output keeps the two in sync.

By [@BrynCooke](https://github.com/BrynCooke)
