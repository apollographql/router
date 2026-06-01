### fix: clamp license timers to a safe limit ([PR #9561](https://github.com/apollographql/router/pull/9561))

Routers with licenses whose `halt_at` date is more than approximately two years in the future would panic on startup with `invalid deadline; err=Invalid`.

The fix clamps `halt_at` and `warn_at` deadlines to a maximum derived from the documented offline license validity window (one year plus grace) plus headroom, kept below Tokio's timer wheel limit, before registering them with the timer queue. In practice this has no observable effect: license checks are refreshed continuously from Uplink, so the clamped timer is always replaced well before it would fire.

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/9561
