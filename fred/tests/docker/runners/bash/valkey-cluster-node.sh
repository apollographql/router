#!/usr/bin/env bash

CLUSTER_HOST=127.0.0.1
TIMEOUT=2000

if [[ -z "${VALKEYCLI_AUTH}" ]]; then
  echo "Skipping authentication checks..."
else
  # it's unclear which of these env variables have been changed to use "valkey" instead of "redis". at the time of
  # writing there still seems to be several references that use the old names.
  export REDIS_PASSWORD="${VALKEYCLI_AUTH}"
  export VALKEY_PASSWORD="${VALKEYCLI_AUTH}"
  export REDISCLI_AUTH="${VALKEYCLI_AUTH}"
fi

function start_server {
  [[ -z "${VALKEYCLI_AUTH}" ]] && AUTH_ARGV="" || AUTH_ARGV="--requirepass ${VALKEYCLI_AUTH} --masterauth ${VALKEYCLI_AUTH}"
  [[ -z "${VALKEY_ACLFILE}" ]] && ACL_ARGV="" || ACL_ARGV="--aclfile ${VALKEY_ACLFILE}"

  valkey-server --port $VALKEY_PORT_NUMBER --cluster-enabled yes --cluster-config-file nodes-${VALKEY_PORT_NUMBER}.conf \
    --cluster-node-timeout $TIMEOUT --appendonly yes --appendfilename appendonly-${VALKEY_PORT_NUMBER}.aof \
    --loglevel verbose --appenddirname appendonlydir-${VALKEY_PORT_NUMBER} --dbfilename dump-${VALKEY_PORT_NUMBER}.rdb \
    --logfile ${VALKEY_PORT_NUMBER}.log --daemonize yes --enable-debug-command yes $AUTH_ARGV $ACL_ARGV

  echo $! > valkey-server.pid
}

function wait_for_server {
  [[ -z "${VALKEYCLI_AUTH}" ]] && AUTH_ARGV="" || AUTH_ARGV="-a ${VALKEYCLI_AUTH}"

  for i in `seq 1 10`; do
    if [[ `valkey-cli -h $CLUSTER_HOST -p $VALKEY_PORT_NUMBER $AUTH_ARGV --raw PING` == "PONG" ]]; then
      return
    else
      sleep 1
    fi
  done

  echo "Timed out waiting for server to start."
  exit 1
}

function create_cluster {
  echo "Creating cluster..."
  [[ -z "${VALKEYCLI_AUTH}" ]] && AUTH_ARGV="" || AUTH_ARGV="-a ${VALKEYCLI_AUTH}"

  HOSTS=""
  for cluster_host in $VALKEY_NODES; do
    HOSTS="$HOSTS $cluster_host:$VALKEY_PORT_NUMBER";
  done

  valkey-cli $AUTH_ARGV --cluster create $HOSTS --cluster-replicas $VALKEY_CLUSTER_REPLICAS --cluster-yes
}

parse_config_file
start_server
wait_for_server
if [[ $VALKEY_CLUSTER_CREATOR == "yes" ]]; then
  create_cluster
fi
# TODO fix this so docker can properly control the valkey server process
tail -f ${VALKEY_PORT_NUMBER}.log
