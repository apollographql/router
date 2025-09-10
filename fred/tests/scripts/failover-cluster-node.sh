#!/bin/bash
. tests/scripts/utils.sh
check_root_dir

argv=("$@")

echo "Putting ${argv[0]} in fail state for ${argv[1]} seconds..."
./tests/tmp/redis_$REDIS_VERSION/redis-$REDIS_VERSION/src/redis-cli -p ${argv[0]} DEBUG sleep ${argv[1]}
