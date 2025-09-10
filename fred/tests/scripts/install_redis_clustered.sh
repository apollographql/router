#!/bin/bash
. tests/scripts/utils.sh

check_root_dir
check_redis
if [[ "$?" -eq 0 ]]; then
  install_redis
fi
start_cluster
echo "Finished installing clustered redis server."