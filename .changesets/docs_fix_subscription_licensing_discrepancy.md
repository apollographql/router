### Fix subscription licensing discrepancy in documentation ([PR #8726](https://github.com/apollographql/router/pull/8726))

Corrected the subscription support documentation to reflect that subscriptions are available on all GraphOS plans (Free, Developer, Standard, and Enterprise) with self-hosted routers.

The documentation previously stated that subscription support was an Enterprise-only feature for self-hosted routers, which was incorrect. Subscriptions are a licensed feature available to all GraphOS plans when the router is connected to GraphOS with an API key and graph ref.

Updated both the configuration and overview pages to remove the misleading Enterprise-only requirement and clarify the actual requirements.

By [@gigi](https://github.com/the-gigi-apollo) in https://github.com/apollographql/router/pull/8726
