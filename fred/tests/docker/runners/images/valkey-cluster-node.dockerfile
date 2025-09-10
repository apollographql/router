# https://github.com/docker/for-mac/issues/5548#issuecomment-1029204019
FROM valkey/valkey:7.2-bookworm
ARG VALKEY_VERSION
ARG VALKEY_PORT_NUMBER
ARG VALKEYCLI_AUTH
ARG VALKEY_NODES
ARG VALKEY_CLUSTER_REPLICAS
ARG VALKEY_CLUSTER_CREATOR
ARG VALKEY_ACLFILE

COPY tests/docker/runners/bash/valkey-cluster-node.sh /usr/bin/
ENTRYPOINT ["valkey-cluster-node.sh"]