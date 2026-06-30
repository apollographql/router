### Deprecate `traffic_shaping.deduplicate_variables` field ([PR #9586](https://github.com/apollographql/router/pull/9586))

The router config field `traffic_shaping.deduplicate_variables` is now deprecated. Since variable deduplication is unconditionally enabled, the field is silently ignored and will be removed. A warning will now be issued at startup when this field is set to alert operators to remove the field from their config.

By [@conwuegb](https://github.com/conwuegb) in https://github.com/apollographql/router/pull/9586
