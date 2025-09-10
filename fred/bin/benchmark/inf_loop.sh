#!/bin/bash

idx=1
while :
do
  echo "Running ($idx)..."
  $@ > server.log 2>&1
  idx=$(( $idx + 1 ))
done