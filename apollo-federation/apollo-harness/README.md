# README

`apollo-harness` provides a mechanism for running different query planning implementations and extracting interesting performance data.

## Supported Platforms
 - macOS m1/m2
 - linux x86

We are using `heaptrack` to generate our results. `heaptrack` is only available on linux, but we'd like to support the widely use macOS m1 platform. We achieve our goal via containerisation and cross-compiling code in
a container (using the `cross` crate).

When you run `scripts/run-tests.sh` for the first time, it will make sure that your system has:
 - some form of container runtime (docker/podman are supported)
 - cargo cross crate
 - rustup toolchain that matches your host platform arch (that's important for when cross is installed)

Once all of these pieces are in place, things should just work. If they don't - let me (slack: @gary) know or file an issue.

# Getting Started

Clone the `federation-next` repo and cd to `apollo-harness`. Some of the scripts make assumptions that you are in this directory, so don't cd away from here.

You'll find there are several sub-directories:

## scripts

The scripts you'll need to run a batch of tests are here.

The only one you will be running is `scripts/run_tests.sh`. This script checks that your execution environment looks ok and provides some advice on installing/configuring required tooling before executing a batch of tests.

## src

This directory contains the source code for building a rust executable which performs schema loading and (optionally) query planning. The only binary right now is `rb_loader` which uses the `router-bridge` to perform these tasks.

When you execute a batch of tests, this code is cross-compiled into a linux container, so that heaptrack can be reliably executed from your host.

You shouldn't need to make any changes to this source code unless you are adding a new federation schema loading query planner. In which case, add a new `[[bin]]` and implement accordingly.

Note: If you are making changes to `src`, you'll need to make sure that `cmake` is installed on your host system since it's required to build `deno-core`.

## testdata

This directory contains a `controlfile`. Any line beginning with `#` is ignored and the format is documented in the file. For example:

```
# Format -> free text title:program name:schema file:query file
load and plan 1:rb_loader:schema.graphql:query.graphql
load 1:rb_loader:schema.graphql:
load 2:rb_loader:mohameds-team.graphql:
load 3:rb_loader:symbiose.graphql:
```

This controlfile will result in a batch of tests which will generate results in the results/[test name] directory. All tests will load a schema file, so the schema file argument is mandatory. You may optionally provide a query to plan.

## results

The results of running each test in the controlfile are generated here. The full output of heaptrack analysis is captured in a compressed timestamped file alongside a summary report which has the same timestamp. For most purposes, the summary file will be enough information to determine if a performance regression has occurred, but if you are investigating a regression, you will probably want to access the full heaptrack data in the compressed data file.

# Interpreting Results

Here's what the output of a typical run looks like:
```
garypen@Garys-MacBook-Pro apollo-harness % ./scripts/run_tests.sh
Using /usr/local/bin/docker for containerisation...
Building target: aarch64-unknown-linux-gnu
This may take some time, especially on your first run...


Results: load_and_plan_1/2023_10_06_11:51:55.out
total runtime (un-instrumented): 0.14s
total runtime: 0.26s.
calls to allocation functions: 130782 (512870/s)
temporary memory allocations: 35598 (139600/s)
peak heap memory consumption: 5.71M
peak RSS (including heaptrack overhead): 62.71M
total memory leaked: 250.41K


Results: load_1/2023_10_06_11:51:55.out
total runtime (un-instrumented): 0.14s
total runtime: 0.22s.
calls to allocation functions: 108628 (489315/s)
temporary memory allocations: 31196 (140522/s)
peak heap memory consumption: 5.64M
peak RSS (including heaptrack overhead): 60.86M
total memory leaked: 250.41K

garypen@Garys-MacBook-Pro apollo-harness % 
```

Most of the data is self explanatory. For our purposes, we are mainly interested in two values:

total runtime (un-instrumented): 
peak RSS (including heaptrack overhead):

These two values tell us, the wall clock for executing the test (measure execution performance regressions) and the peak amount of resident memory (measure memory resource consumption regressions).

# TODO

1. Write a baseline comparison and check in some baseline data.
