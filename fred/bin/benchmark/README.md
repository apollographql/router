Fred Benchmark
==============

Redis includes a [benchmarking tool](https://redis.io/docs/management/optimization/benchmarks/) that can be used to
measure the throughput of a client/connection pool. This module attempts to reproduce the same process with Tokio and
Fred.

The general strategy involves using an atomic global counter and spawning `-c` Tokio tasks that share `-P` clients in
order to send `-n` total `INCR` commands to the server as quickly as possible.

Each of the `-c` Tokio tasks use a different random key so commands are uniformly distributed across a cluster or
replica set.

This strategy also has the benefit of being somewhat representative of an Axum or Actix web server use case where
requests run in separate Tokio tasks but share a common client pool.

The [benchmark metrics](../benchmark_metrics) folder contains a tool that can test different combinations of
concurrency (`-c`) and pool size (`-P`) argv.

## Tuning

There are several additional features or performance tuning options that can affect these results. For example:

* Tracing. Enabling the FF cut throughput by ~20% in my tests.
* Clustering
* Backpressure settings
* Network latency
* Log levels, often indirectly for the same reason as `tracing` (contention on a pipe, file handle, or socket).
* The size of the client connection pool.

Callers should take care to consider each of these when deciding on argv values.

This module also includes an optional `assert-expected` feature flag that adds an `assert!` call after each `INCR`
command to ensure the response is actually correct.

## Tracing

This also shows how to configure the client with tracing enabled against a local Jaeger instance.
A [docker compose](../../tests/docker/compose/jaeger.yml) file is included that will run a local Jaeger instance.

```
docker-compose -f /path/to/fred/tests/docker/compose/jaeger.yml up
```

Then navigate to <http://localhost:16686>.

By default, this module does not compile any tracing features, but there are 3 flags that can toggle how tracing is
configured.

* `partial-tracing` - Enables `fred/partial-tracing` and emits traces to the local jaeger instance.
* `full-tracing` - Enables `fred/full-tracing` and emits traces to the local jaeger instance.
* `stdout-tracing` - Enables `fred/partial-tracing` and emits traces to stdout.

## Docker

Linux+Docker is the best supported option via the `./run.sh` script. The `Cargo.toml` provided here has a comment/toggle
around the lines that need to change if callers want to use a remote server.

Callers may have to also change `run.sh` to enable additional features in docker.

## Usage

```
A benchmarking module based on the `redis-benchmark` tool included with Redis.

USAGE:
    fred_benchmark [FLAGS] [OPTIONS]

FLAGS:
        --cluster     Whether to assume a clustered deployment.
        --help        Prints help information
    -q, --quiet       Only print the final req/sec measurement.
        --replicas    Whether to use `GET` with replica nodes instead of `INCR` with primary nodes.
    -t, --tls         Enable TLS via whichever build flag is provided.
    -T, --tracing     Whether to enable tracing via a local Jeager instance. See tests/docker-compose.yml to start up a
                      local Jaeger instance.
    -V, --version     Prints version information

OPTIONS:
    -a, --auth <STRING>           The password/key to use. `REDIS_USERNAME` and `REDIS_PASSWORD` can also be used.
        --bounded <NUMBER>        The size of the bounded mpsc channel used to route commands. [default: 0]
    -c, --concurrency <NUMBER>    The number of Tokio tasks used to run commands. [default: 100]
    -n, --commands <NUMBER>       The number of commands to run. [default: 100000]
    -h, --host <STRING>           The hostname of the redis server. [default: 127.0.0.1]
    -P, --pool <NUMBER>           The number of clients in the redis connection pool. [default: 1]
    -p, --port <NUMBER>           The port for the redis server. [default: 6379]
    -u, --unix-sock <PATH>        The path to a unix socket.
```

## Examples

All the examples below use the following parameters:

* Clustered deployment via local docker (3 primary nodes with one replica each)
* No tracing features enabled
* No TLS
* 10_000_000 INCR commands with `assert-expected` enabled
* 10_000 Tokio tasks
* 15 clients in the connection pool

```
$ ./run.sh --cluster -c 10000 -n 10000000 -P 15 -h redis-cluster-1 -p 30001 -a bar
Performed 10000000 operations in: 3.337158005s. Throughput: 2996703 req/sec
```

Using `GET` with replica nodes instead of `INCR` with primary nodes:

```
$ ./run.sh --cluster -c 10000 -n 10000000 -P 15 -h redis-cluster-1 -p 30001 -a bar --replicas
Performed 10000000 operations in: 1.865807963s. Throughput: 5361930 req/sec
```

Relevant Specs:

* 32 CPUs
* 64 GB memory