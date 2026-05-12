### Router instances no longer get permanently stuck on stale schema after a transient reload failure ([PR #9391](https://github.com/apollographql/router/pull/9391))

Previously, if a schema reload failed — for example, because a persisted query manifest fetch from Uplink encountered a transient network error — the router would log "error while reloading, continuing with previous configuration" and then stop retrying. All subsequent background polls from Uplink would return `Unchanged` (because `last_id` had already advanced to the new schema ID), leaving the router permanently serving the old schema until manually restarted.

The router now enters a `Reloading` state on reload failure and schedules automatic retries. The retry delay (default 10 seconds) and maximum retry count (default 5) are configurable via the new `reload` configuration key:

```yaml
reload:
  max_retries: 5    # 0 to disable, null for unlimited
  retry_delay: 10s
```

The retry budget is reset whenever a new reload trigger arrives — a new schema or license from Uplink, a configuration or rhai script file change, or an explicit reload signal — so any new change always gets a fresh set of attempts even if previous retries were exhausted.

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/9391
