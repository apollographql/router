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

echo "Stopping redis processes..."
pgrep redis | sudo xargs kill -9

echo "Stopping sentinel redis in docker..."
docker-compose -f ./tests/sentinel-docker-compose.yml stop