#!/bin/sh
#
# In case of the error below when adding or changing an SQL query,
# run this script to update their metadata.
#
#    error: `SQLX_OFFLINE=true` but there is no cached data for this query

cd $(dirname $0)/../apollo-router
cargo sqlx prepare -D postgres://localhost -- --all-targets
