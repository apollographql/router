### Fix healthcheck readiness ticker resetting the sampling window clock on recovery ([PR #8966](https://github.com/apollographql/router/pull/8966))

The readiness ticker recreated its `interval_at` inside the recovery loop on every cycle, which reset the sampling window clock each time the router returned to ready. This caused inconsistent sampling behavior across recovery cycles. Additionally, the rejection counter was read and zeroed with separate atomic operations (`load` + `store(0)`), leaving a race window where rejections arriving between the two operations could be silently dropped.

The fix creates the interval once outside the loop, sets `MissedTickBehavior::Delay` to prevent catch-up ticks after the recovery sleep, and uses `swap(0, Relaxed)` to atomically read and reset the rejection counter in a single operation.

By [@OriginLeon](https://github.com/OriginLeon) in https://github.com/apollographql/router/pull/8966
