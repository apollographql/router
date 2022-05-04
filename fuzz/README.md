# Fuzzy testing

## Targets

### Router

This target is especially testing if we have differences between gateway 2 responses and router responses. As soon as it detects a difference it panics and write a report in `router_error.txt`.
Before launching it you have to spawn the docker-compose for federation 2 with `docker-compose -f docker-compose-federation2.yml up` at the root of the router directory and also run a local router listening on port `4000`.
And then run it with:

```
# Only works on Linux
cargo +nightly fuzz run router
```

### Federation

This target is useful to spot differences between `gateway@1.x` and `gateway@2.x`. Before launching it you have to spawn the docker-compose located in the `fuzz` directory: `docker-compose -f fuzz/docker-compose.yml up`.
And then run it with:

```
# Only works on Linux
cargo +nightly fuzz run federation
```