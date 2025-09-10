```
Run a script that tests cluster consistency.

USAGE:
    replica_consistency [FLAGS] [OPTIONS]

FLAGS:
        --help       Prints help information
    -V, --version    Prints version information
        --wait       Whether to send `WAIT 1 10` after each `SET` operation

OPTIONS:
    -a, --auth <STRING>           An optional authentication key or password. [default: ]
    -c, --concurrency <NUMBER>    The number of concurrent set-get commands to set each `interval`. [default: 500]
    -h, --host <STRING>           The hostname of the redis server. [default: 127.0.0.1]
    -i, --interval <NUMBER>       The time to wait between commands in milliseconds. [default: 500]
    -P, --pool <NUMBER>           The number of clients in the redis connection pool. [default: 1]
    -p, --port <NUMBER>           The port for the redis server. [default: 6379]
```

```
cd path/to/fred
source ./tests/environ
cd bin/replica_consistency
RUST_LOG=replica_consistency=info,fred=debug ./run.sh -a bar -h redis-cluster-1 -p 30001 -P 6 -i 500 -c 500
```