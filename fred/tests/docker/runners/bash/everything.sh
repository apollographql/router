#!/bin/bash

./tests/runners/default-features.sh \
  && ./tests/runners/no-features.sh \
  && ./tests/runners/all-features.sh \
  && ./tests/runners/sentinel-features.sh