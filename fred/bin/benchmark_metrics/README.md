Benchmark Metrics
=================

Two of the more interesting variables for performance tuning purposes are the number of Tokio tasks (`--concurrency`
or `-c`) and the size of the client pool (`pool` or `-P`). This module will repeatedly run the [benchmark](../benchmark)
tool with different combinations of these two inputs.

```
./run.sh -h redis-cluster-1 -p 30001 --cluster -n 1000000 -P 1-16 --pool-step 2 -c 50-10000 --concurrency-step 50 pipeline
```

This will run the benchmark module with sensible combinations of argv in the ranges provided. The output CSV file maps
these argv combinations to their benchmarked throughput value.

## Usage

```
USAGE:
    benchmark_metrics [FLAGS] [OPTIONS] [SUBCOMMAND]

FLAGS:
        --cluster    Whether to assume a clustered deployment.
        --help       Prints help information
    -V, --version    Prints version information

OPTIONS:
    -c, --concurrency <RANGE (X-Y)>    The number of concurrent tasks used to run commands. [default: 1-1000]
        --concurrency-step <NUMBER>    The amount to increment the `concurrency` value on each test run. [default: 30]
    -n, --commands <NUMBER>            The number of commands to run. [default: 10000]
    -h, --host <STRING>                The hostname of the redis server. [default: 127.0.0.1]
    -P, --pool <RANGE (X-Y)>           The number of clients in the redis connection pool. [default: 1-50]
        --pool-step <NUMBER>           The amount to increment the `pool` value on each test run. [default: 2]
    -p, --port <NUMBER>                The port for the redis server. [default: 6379]

SUBCOMMANDS:
    help           Prints this message or the help of the given subcommand(s)
    no-pipeline    Run the test without pipelining.
    pipeline       Run the test with pipelining.
```

## Docker

See the [benchmark](../benchmark/README.md#docker) section for more information. Many of the same restrictions apply.

## Examples

The [metrics](./metrics) folder has the results of running this on my desktop with the following parameters:

* 32 CPUs, 64 GB, Ubuntu
* Clustered deployment running via local docker (3 primary nodes with one replica each)
* Centralized deployment running via local docker (no replicas)
* No tracing features enabled
* No TLS
* 1_000_000 INCR commands with `assert-expected` enabled
* Between 100 and 15_000 Tokio tasks, with increments of 100
* 1 - 20 clients in the connection pool, with increments of 2

To regenerate the `metrics` folder (assuming Docker on Linux):

```
# don't forget the `source /path/to/fred/tests/environ` step

# warning: it can be difficult to debug issues through multiple layers of docker. i'd recommend testing the underlying 
# commands individually before running this since this script takes hours and does not show output while running.

./run.sh -h redis-cluster-1 -p 30001 --cluster -n 1000000 -P 1-20 --pool-step 2 -c 100-15000 --concurrency-step 100 pipeline > ./metrics/clustered/pipeline.csv \
  && ./run.sh -h redis-cluster-1 -p 30001 --cluster -n 1000000 -P 1-20 --pool-step 2 -c 100-15000 --concurrency-step 100 no-pipeline > ./metrics/clustered/no-pipeline.csv \
  && ./run.sh -h redis-main -p 6379 -n 1000000 -P 1-20 --pool-step 2 -c 100-15000 --concurrency-step 100 pipeline > ./metrics/centralized/pipeline.csv \
  && ./run.sh -h redis-main -p 6379 -n 1000000 -P 1-20 --pool-step 2 -c 100-15000 --concurrency-step 100 no-pipeline > ./metrics/centralized/no-pipeline.csv
```


