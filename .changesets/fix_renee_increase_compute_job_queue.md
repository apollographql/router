### Increase compute job worker pool queue size ([PR #7205](https://github.com/apollographql/router/pull/7205))

The compute job worker pool is used for CPU-bound tasks, like GraphQL parsing, validation, and query planning. When there are too many jobs to handle in parallel, jobs enter a queue.

We previously set this queue size to 20 (per thread) somewhat arbitrarily. We got some signals that this may be too small.

This patch increases the queue size to 1 000 jobs per thread. For reference, in older router versions before the introduction of the compute job worker pool, the equivalent queue size was *10 000*.

The number is still a bit arbitrary, and subject to more changes in the future as we understand its effects better. Along with some other tweaks to job priorities we expect this to give better behaviour and reject fewer requests needlessly.


By [@goto-bus-stop](https://github.com/goto-bus-stop) in https://github.com/apollographql/router/pull/7205