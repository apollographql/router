#!/bin/bash
. tests/scripts/utils.sh


check_root_dir
check_redis
if [[ "$?" -eq 0 ]]; then
  install_redis
fi
configure_centralized_acl
start_centralized

echo "Finished installing centralized redis server."