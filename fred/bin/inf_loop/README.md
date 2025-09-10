Inf Loop
========

A simple test script that sends `INCR foo` via a `RedisPool` on an interval forever using an infinite reconnect policy.

```
USAGE:
    inf_loop [FLAGS] [OPTIONS]

FLAGS:
        --cluster    Whether to use a clustered deployment.
        --help       Prints help information
    -V, --version    Prints version information

OPTIONS:
    -h, --host <STRING>        The hostname of the redis server. [default: 127.0.0.1]
    -i, --interval <NUMBER>    The time to wait between INCR commands in milliseconds. [default: 1000]
    -P, --pool <NUMBER>        The number of clients in the redis connection pool. [default: 1]
    -p, --port <NUMBER>        The port for the redis server. [default: 6379]
    -w, --wait <NUMBER>        Add a delay, in milliseconds, after connecting but before starting the INCR loop.
                               [default: 0]
```

If using docker:

```
./run.sh --cluster -h redis-cluster-1 -p 30001 -a key
```