#!/bin/sh
set -eu
# PING health does not imply cluster convergence after a restart.
info=$(timeout 5 redis-cli -h redis-1 cluster info)
slots=$(printf '%s\n' "$info" | sed -n 's/^cluster_slots_assigned://p' | tr -d '\r')
known=$(printf '%s\n' "$info" | sed -n 's/^cluster_known_nodes://p' | tr -d '\r')
if [ "$slots" = "0" ] && [ "$known" = "1" ]; then
    prefix="${REDIS_NETWORK_PREFIX:-172.30.33}"
    timeout 60 redis-cli --cluster create \
        "$prefix".11:6379 "$prefix".12:6379 "$prefix".13:6379 \
        "$prefix".14:6379 "$prefix".15:6379 "$prefix".16:6379 \
        --cluster-replicas 1 --cluster-yes
else
    echo "Existing Redis Cluster: waiting for convergence"
fi
for attempt in $(seq 1 60); do
    ready=true
    timeout 3 redis-cli -h redis-1 cluster info | grep -q 'cluster_state:ok' || ready=false
    for node in redis-1 redis-2 redis-3 redis-4 redis-5 redis-6; do
        replication=$(timeout 3 redis-cli -h "$node" info replication) || { ready=false; continue; }
        if printf '%s\n' "$replication" | grep -q 'role:slave'; then
            printf '%s\n' "$replication" | grep -q 'master_link_status:up' || ready=false
            printf '%s\n' "$replication" | grep -q 'master_sync_in_progress:0' || ready=false
        else
            printf '%s\n' "$replication" | grep -q 'connected_slaves:1' || ready=false
            printf '%s\n' "$replication" | grep -q 'state=online' || ready=false
        fi
    done
    if [ "$ready" = "true" ]; then echo "Cluster and all replicas are ready"; exit 0; fi
    sleep 1
done
echo "Redis Cluster or its replicas did not become ready" >&2
exit 1
