### Apply configuration migrations at startup and reload

The router now applies major-version migrations at startup and reload, including renaming `experimental_batching` to `batching`. When a migration changes the configuration, run `router config upgrade` and save the reviewed output to update your file on disk. Validation errors refer to the migrated configuration's line numbers and snippets. Configuration that remains invalid after migration prevents startup or rejects the reload.

By [@BrynCooke](https://github.com/BrynCooke)
