#!/bin/bash

# may need to run as root.
# echo 0 > /proc/sys/kernel/kptr_restrict

cargo build --release --features assert-expected
valgrind --tool=callgrind target/release/fred_benchmark -h 127.0.0.1 -p 6379 -a bar -n 100000 -P 1 -q -c 10000 pipeline
kcachegrind