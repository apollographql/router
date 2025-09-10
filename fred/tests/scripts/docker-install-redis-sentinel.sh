#!/bin/bash

if [ ! -d "./tests/tmp" ]; then
  echo "Must be in application root for redis installation scripts to work."
  exit 1
fi

declare -a arr=("REDIS_VERSION" "REDIS_USERNAME" "REDIS_PASSWORD" "REDIS_SENTINEL_PASSWORD")

for env in "${arr[@]}"
do
  if [ -z "$env" ]; then
    echo "$env must be set. Run `source tests/environ` if needed."
    exit 1
  fi
done

echo "Note: this requires docker, docker-compose, and redis >=6.2 to work reliably."
docker-compose -f ./tests/sentinel-docker-compose.yml up -d
