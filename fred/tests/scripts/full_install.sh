#!/bin/bash

declare -a arr=("REDIS_VERSION" "REDIS_USERNAME" "REDIS_PASSWORD" "REDIS_SENTINEL_PASSWORD")

for env in "${arr[@]}"
do
  if [ -z "$env" ]; then
    echo "$env must be set. Run `source tests/environ` if needed."
    exit 1
  fi
done

if [ ! -d "./tests/tmp" ]; then
  echo "Must be in application root for installation script to work."
  exit 1
fi

./tests/scripts/stop_all_redis.sh
rm -rf ./tests/tmp/redis*
./tests/scripts/install_redis_centralized.sh
./tests/scripts/install_redis_clustered.sh
./tests/scripts/docker-install-redis-sentinel.sh