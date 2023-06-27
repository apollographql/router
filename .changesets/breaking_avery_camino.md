### All file paths must be valid UTF-8

All file paths defined in `router.yaml` configuration or via CLI options must be valid UTF-8. This may break your setup if you are storing router configuration files at file paths that are not valid UTF-8, otherwise behavior is not impacted. Restricting invalid UTF-8 paths makes working with file paths internally much easier as they can always be represented as proper strings as opposed to doing a lossy conversion every time a file path needs to be displayed.

By [@EverlastingBugstopper](https://github.com/EverlastingBugstopper) in https://github.com/apollographql/router/pull/3314