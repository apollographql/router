#!/bin/bash

docker-compose -f tests/docker/compose/glommio.yml run -u $(id -u ${USER}):$(id -g ${USER}) --rm glommio

