#[cfg(feature = "i-keys")]
mod keys {
  cluster_test!(keys, should_handle_missing_keys);
  cluster_test!(keys, should_set_and_get_a_value);
  cluster_test!(keys, should_set_and_del_a_value);
  cluster_test!(keys, should_set_with_get_argument);
  cluster_test!(keys, should_incr_and_decr_a_value);
  cluster_test!(keys, should_incr_by_float);
  cluster_test!(keys, should_mset_a_non_empty_map);
  cluster_test_panic!(keys, should_error_mset_empty_map);
  cluster_test!(keys, should_expire_key);
  cluster_test!(keys, should_pexpire_key);
  cluster_test!(keys, should_persist_key);
  cluster_test!(keys, should_check_ttl);
  cluster_test!(keys, should_check_pttl);
  cluster_test!(keys, should_dump_key);
  cluster_test!(keys, should_dump_and_restore_key);
  cluster_test!(keys, should_modify_ranges);
  cluster_test!(keys, should_getset_value);
  cluster_test!(keys, should_getdel_value);
  cluster_test!(keys, should_get_strlen);
  cluster_test!(keys, should_mget_values);
  cluster_test!(keys, should_msetnx_values);
  cluster_test!(keys, should_copy_values);

  cluster_test!(keys, should_unlink);
  cluster_test_panic!(keys, should_error_rename_does_not_exist);
  cluster_test_panic!(keys, should_error_renamenx_does_not_exist);
  cluster_test!(keys, should_rename);
  cluster_test!(keys, should_renamenx);
  cluster_test!(keys, should_expire_time_value);
  cluster_test!(keys, should_pexpire_time_value);
  #[cfg(all(feature = "i-keys", feature = "i-hashes", feature = "i-sets"))]
  cluster_test!(keys, should_check_type_of_key);

  cluster_test!(keys, should_get_keys_from_pool_in_a_stream);
}

#[cfg(all(feature = "transactions", feature = "i-keys"))]
mod multi {
  cluster_test!(multi, should_run_get_set_trx);
  cluster_test_panic!(multi, should_fail_with_hashslot_error);
  cluster_test_panic!(multi, should_run_error_get_set_trx);
}

mod other {
  cluster_test!(other, should_connect_correctly_via_init_interface);
  cluster_test!(other, should_fail_with_bad_host_via_init_interface);
  cluster_test!(other, should_connect_correctly_via_wait_interface);
  cluster_test!(other, should_fail_with_bad_host_via_wait_interface);
  cluster_test!(other, pool_should_connect_correctly_via_init_interface);
  cluster_test!(other, pool_should_fail_with_bad_host_via_init_interface);
  cluster_test!(other, pool_should_connect_correctly_via_wait_interface);
  cluster_test!(other, pool_should_fail_with_bad_host_via_wait_interface);
  cluster_test!(other, should_split_clustered_connection);
  cluster_test!(other, should_safely_change_protocols_repeatedly);
  cluster_test!(other, should_gracefully_quit);
  cluster_test!(other, should_not_hang_on_concurrent_quit);

  #[cfg(feature = "i-pubsub")]
  cluster_test!(other, should_exit_event_task_with_error);
  #[cfg(all(feature = "transactions", feature = "i-keys", feature = "i-hashes"))]
  cluster_test!(other, should_fail_pipeline_transaction_error);
  #[cfg(all(feature = "transactions", feature = "i-keys"))]
  cluster_test!(other, should_pipeline_transaction);
  #[cfg(feature = "credential-provider")]
  cluster_test!(other, should_use_credential_provider);
  #[cfg(feature = "metrics")]
  cluster_test!(other, should_track_size_stats);
  #[cfg(feature = "i-server")]
  cluster_test!(other, should_run_flushall_cluster);
  #[cfg(all(feature = "i-client", feature = "i-lists"))]
  cluster_test!(other, should_automatically_unblock);
  #[cfg(all(feature = "i-client", feature = "i-lists"))]
  cluster_test!(other, should_manually_unblock);
  #[cfg(all(feature = "i-client", feature = "i-lists"))]
  cluster_test!(other, should_error_when_blocked);
  #[cfg(feature = "i-keys")]
  cluster_test!(other, should_pipeline_all);
  #[cfg(all(feature = "i-keys", feature = "i-hashes"))]
  cluster_test!(other, should_pipeline_all_error_early);
  #[cfg(feature = "i-keys")]
  cluster_test!(other, should_pipeline_last);
  #[cfg(all(feature = "i-keys", feature = "i-hashes"))]
  cluster_test!(other, should_pipeline_try_all);
  #[cfg(feature = "i-server")]
  cluster_test!(other, should_use_all_cluster_nodes_repeatedly);
  #[cfg(feature = "i-lists")]
  cluster_test!(other, should_support_options_with_pipeline);
  #[cfg(feature = "i-keys")]
  cluster_test!(other, should_reuse_pipeline);
  #[cfg(all(feature = "i-keys", feature = "i-lists"))]
  cluster_test!(other, should_manually_connect_twice);
  #[cfg(all(feature = "transactions", feature = "i-keys"))]
  cluster_test!(other, should_mix_trx_and_get);
  #[cfg(all(feature = "transactions", feature = "i-keys"))]
  cluster_test!(other, should_support_options_with_trx);

  //#[cfg(feature = "dns")]
  // cluster_test!(other, should_use_trust_dns);
  #[cfg(all(feature = "partial-tracing", feature = "i-keys"))]
  cluster_test!(other, should_use_tracing_get_set);
  #[cfg(feature = "subscriber-client")]
  cluster_test!(other, should_ping_with_subscriber_client);

  #[cfg(all(feature = "replicas", feature = "i-keys"))]
  cluster_test!(other, should_replica_set_and_get);
  #[cfg(all(feature = "replicas", feature = "i-keys"))]
  cluster_test!(other, should_replica_set_and_get_not_lazy);
  #[cfg(feature = "replicas")]
  cluster_test!(other, should_create_non_lazy_replica_connections);
  #[cfg(all(feature = "replicas", feature = "i-keys"))]
  cluster_test!(other, should_use_cluster_replica_without_redirection);
  #[cfg(all(feature = "replicas", feature = "i-keys"))]
  cluster_test!(other, should_combine_options_and_replicas_non_lazy);
  #[cfg(all(feature = "replicas", feature = "i-keys"))]
  cluster_test!(other, should_combine_options_and_replicas);
  #[cfg(all(feature = "replicas", feature = "i-keys"))]
  cluster_test!(other, should_pipeline_with_replicas);
}

mod pool {
  cluster_test!(pool, should_connect_and_ping_static_pool_single_conn);
  cluster_test!(pool, should_connect_and_ping_static_pool_two_conn);
  #[cfg(feature = "i-keys")]
  cluster_test!(pool, should_incr_exclusive_pool);
  #[cfg(all(feature = "i-keys", feature = "transactions"))]
  cluster_test!(pool, should_watch_and_trx_exclusive_pool);
}

#[cfg(feature = "i-hashes")]
mod hashes {
  cluster_test!(hashes, should_hset_and_hget);
  cluster_test!(hashes, should_hset_and_hdel);
  cluster_test!(hashes, should_hexists);
  cluster_test!(hashes, should_hgetall);
  cluster_test!(hashes, should_hincryby);
  cluster_test!(hashes, should_hincryby_float);
  cluster_test!(hashes, should_get_keys);
  cluster_test!(hashes, should_hmset);
  cluster_test!(hashes, should_hmget);
  cluster_test!(hashes, should_hsetnx);
  cluster_test!(hashes, should_get_random_field);
  cluster_test!(hashes, should_get_strlen);
  cluster_test!(hashes, should_get_values);
  #[cfg(feature = "i-hexpire")]
  cluster_test!(hashes, should_do_hash_expirations);
  #[cfg(feature = "i-hexpire")]
  cluster_test!(hashes, should_do_hash_pexpirations);
}

#[cfg(feature = "i-pubsub")]
mod pubsub {
  cluster_test!(pubsub, should_publish_and_recv_messages);
  cluster_test!(pubsub, should_ssubscribe_and_recv_messages);
  cluster_test!(pubsub, should_psubscribe_and_recv_messages);
  cluster_test!(pubsub, should_unsubscribe_from_all);

  // TODO fix these tests so they work with clusters. the connection management logic could be better.
  // cluster_test!(pubsub, should_get_pubsub_channels);
  // cluster_test!(pubsub, should_get_pubsub_numpat);
  // cluster_test!(pubsub, should_get_pubsub_nunmsub);
  cluster_test!(pubsub, should_get_pubsub_shard_channels);
  cluster_test!(pubsub, should_get_pubsub_shard_numsub);
}

#[cfg(feature = "i-hyperloglog")]
mod hyperloglog {
  cluster_test!(hyperloglog, should_pfadd_elements);
  cluster_test!(hyperloglog, should_pfcount_elements);
  cluster_test!(hyperloglog, should_pfmerge_elements);
}

mod scanning {
  #[cfg(feature = "i-keys")]
  cluster_test!(scanning, should_scan_keyspace);
  #[cfg(feature = "i-hashes")]
  cluster_test!(scanning, should_hscan_hash);
  #[cfg(feature = "i-sets")]
  cluster_test!(scanning, should_sscan_set);
  #[cfg(feature = "i-sorted-sets")]
  cluster_test!(scanning, should_zscan_sorted_set);
  #[cfg(feature = "i-keys")]
  cluster_test!(scanning, should_scan_cluster);
  #[cfg(feature = "i-keys")]
  cluster_test!(scanning, should_scan_buffered);
  #[cfg(feature = "i-keys")]
  cluster_test!(scanning, should_scan_cluster_buffered);
  #[cfg(feature = "i-keys")]
  cluster_test!(scanning, should_continue_scanning_on_page_drop);
  #[cfg(all(feature = "i-keys", feature = "i-cluster"))]
  cluster_test!(scanning, should_scan_by_page_clustered);
}

#[cfg(feature = "i-slowlog")]
mod slowlog {
  cluster_test!(slowlog, should_read_slowlog_length);
  cluster_test!(slowlog, should_read_slowlog_entries);
  cluster_test!(slowlog, should_reset_slowlog);
}

#[cfg(feature = "i-server")]
mod server {
  cluster_test!(server, should_flushall);
  cluster_test!(server, should_read_server_info);
  cluster_test!(server, should_ping_pong_command);
  cluster_test!(server, should_read_last_save);
  cluster_test!(server, should_read_db_size);
  cluster_test!(server, should_start_bgsave);
  cluster_test!(server, should_do_bgrewriteaof);
}

#[cfg(feature = "i-sets")]
mod sets {
  cluster_test!(sets, should_sadd_elements);
  cluster_test!(sets, should_scard_elements);
  cluster_test!(sets, should_sdiff_elements);
  cluster_test!(sets, should_sdiffstore_elements);
  cluster_test!(sets, should_sinter_elements);
  cluster_test!(sets, should_sinterstore_elements);
  cluster_test!(sets, should_check_sismember);
  cluster_test!(sets, should_check_smismember);
  cluster_test!(sets, should_read_smembers);
  cluster_test!(sets, should_smove_elements);
  cluster_test!(sets, should_spop_elements);
  cluster_test!(sets, should_get_random_member);
  cluster_test!(sets, should_remove_elements);
  cluster_test!(sets, should_sunion_elements);
  cluster_test!(sets, should_sunionstore_elements);
}

#[cfg(feature = "i-memory")]
pub mod memory {
  cluster_test!(memory, should_run_memory_doctor);
  cluster_test!(memory, should_run_memory_malloc_stats);
  cluster_test!(memory, should_run_memory_purge);
  cluster_test!(memory, should_run_memory_stats);
  cluster_test!(memory, should_run_memory_usage);
}

#[cfg(feature = "i-scripts")]
pub mod lua {
  #[cfg(feature = "sha-1")]
  cluster_test!(lua, should_load_script);
  #[cfg(feature = "sha-1")]
  cluster_test!(lua, should_load_script_cluster);
  cluster_test!(lua, should_eval_echo_script);
  #[cfg(feature = "sha-1")]
  cluster_test!(lua, should_eval_get_script);
  #[cfg(feature = "sha-1")]
  cluster_test!(lua, should_evalsha_echo_script);
  #[cfg(feature = "sha-1")]
  cluster_test!(lua, should_evalsha_with_reload_echo_script);
  #[cfg(feature = "sha-1")]
  cluster_test!(lua, should_evalsha_get_script);

  cluster_test!(lua, should_function_load_scripts);
  cluster_test!(lua, should_function_dump_and_restore);
  cluster_test!(lua, should_function_flush);
  cluster_test!(lua, should_function_delete);
  cluster_test!(lua, should_function_list);
  cluster_test!(lua, should_function_list_multiple);
  #[cfg(feature = "i-keys")]
  cluster_test!(lua, should_function_fcall_getset);
  cluster_test!(lua, should_function_fcall_echo);
  cluster_test!(lua, should_function_fcall_ro_echo);

  #[cfg(feature = "sha-1")]
  cluster_test!(lua, should_create_lua_script_helper_from_code);
  #[cfg(feature = "sha-1")]
  cluster_test!(lua, should_create_lua_script_helper_from_hash);
  cluster_test!(lua, should_create_function_from_code);
  cluster_test!(lua, should_create_function_from_name);
}

#[cfg(feature = "i-sorted-sets")]
pub mod sorted_sets {
  cluster_test!(sorted_sets, should_bzpopmin);
  cluster_test!(sorted_sets, should_bzpopmax);
  cluster_test!(sorted_sets, should_zadd_values);
  cluster_test!(sorted_sets, should_zcard_values);
  cluster_test!(sorted_sets, should_zcount_values);
  cluster_test!(sorted_sets, should_zdiff_values);
  cluster_test!(sorted_sets, should_zdiffstore_values);
  cluster_test!(sorted_sets, should_zincrby_values);
  cluster_test!(sorted_sets, should_zinter_values);
  cluster_test!(sorted_sets, should_zinterstore_values);
  cluster_test!(sorted_sets, should_zlexcount);
  cluster_test!(sorted_sets, should_zpopmax);
  cluster_test!(sorted_sets, should_zpopmin);
  cluster_test!(sorted_sets, should_zrandmember);
  cluster_test!(sorted_sets, should_zrangestore_values);
  cluster_test!(sorted_sets, should_zrangebylex);
  cluster_test!(sorted_sets, should_zrevrangebylex);
  cluster_test!(sorted_sets, should_zrangebyscore);
  cluster_test!(sorted_sets, should_zrevrangebyscore);
  cluster_test!(sorted_sets, should_zrank_values);
  cluster_test!(sorted_sets, should_zrank_values_withscore);
  cluster_test!(sorted_sets, should_zrem_values);
  cluster_test!(sorted_sets, should_zremrangebylex);
  cluster_test!(sorted_sets, should_zremrangebyrank);
  cluster_test!(sorted_sets, should_zremrangebyscore);
  cluster_test!(sorted_sets, should_zrevrank_values);
  cluster_test!(sorted_sets, should_zscore_values);
  cluster_test!(sorted_sets, should_zunion_values);
  cluster_test!(sorted_sets, should_zunionstore_values);
  cluster_test!(sorted_sets, should_zmscore_values);
  cluster_test!(sorted_sets, should_zrangebyscore_neg_infinity);
}

#[cfg(feature = "i-lists")]
pub mod lists {
  cluster_test!(lists, should_blpop_values);
  cluster_test!(lists, should_brpop_values);
  cluster_test!(lists, should_brpoplpush_values);
  cluster_test!(lists, should_blmove_values);
  cluster_test!(lists, should_lindex_values);
  cluster_test!(lists, should_linsert_values);
  cluster_test!(lists, should_lpop_values);
  cluster_test!(lists, should_lpos_values);
  cluster_test!(lists, should_lpush_values);
  cluster_test!(lists, should_lpushx_values);
  cluster_test!(lists, should_lrange_values);
  cluster_test!(lists, should_lrem_values);
  cluster_test!(lists, should_lset_values);
  #[cfg(feature = "i-keys")]
  cluster_test!(lists, should_ltrim_values);
  cluster_test!(lists, should_rpop_values);
  cluster_test!(lists, should_rpoplpush_values);
  cluster_test!(lists, should_lmove_values);
  cluster_test!(lists, should_rpush_values);
  cluster_test!(lists, should_rpushx_values);
  cluster_test!(lists, should_sort_int_list);
  cluster_test!(lists, should_sort_alpha_list);
  cluster_test!(lists, should_sort_int_list_with_limit);
  #[cfg(feature = "replicas")]
  cluster_test!(lists, should_sort_ro_int_list);
}

#[cfg(feature = "i-geo")]
pub mod geo {
  cluster_test!(geo, should_geoadd_values);
  cluster_test!(geo, should_geohash_values);
  cluster_test!(geo, should_geopos_values);
  cluster_test!(geo, should_geodist_values);
  cluster_test!(geo, should_georadius_values);
  cluster_test!(geo, should_georadiusbymember_values);
  cluster_test!(geo, should_geosearch_values);
}

#[cfg(all(not(feature = "i-redis-stack"), feature = "i-acl"))]
pub mod acl {
  cluster_test!(acl, should_run_acl_getuser);
}

#[cfg(feature = "i-streams")]
mod streams {
  cluster_test!(streams, should_xinfo_consumers);
  cluster_test!(streams, should_xinfo_groups);
  cluster_test!(streams, should_xinfo_streams);
  cluster_test!(streams, should_xadd_auto_id_to_a_stream);
  cluster_test!(streams, should_xadd_manual_id_to_a_stream);
  cluster_test!(streams, should_xadd_with_cap_to_a_stream);
  cluster_test!(streams, should_xadd_nomkstream_to_a_stream);
  cluster_test!(streams, should_xtrim_a_stream_approx_cap);
  cluster_test!(streams, should_xtrim_a_stream_eq_cap);
  cluster_test!(streams, should_xdel_one_id_in_a_stream);
  cluster_test!(streams, should_xdel_multiple_ids_in_a_stream);
  cluster_test!(streams, should_xrange_no_count);
  cluster_test!(streams, should_xrange_with_count);
  cluster_test!(streams, should_xrange_values_no_count);
  cluster_test!(streams, should_xrevrange_no_count);
  cluster_test!(streams, should_xrevrange_with_count);
  cluster_test!(streams, should_xrevrange_values_no_count);
  cluster_test!(streams, should_run_xlen_on_stream);
  cluster_test!(streams, should_xread_one_key_count_1);
  cluster_test!(streams, should_xread_multiple_keys_count_2);
  cluster_test!(streams, should_xread_with_blocking);
  cluster_test!(streams, should_xread_map_one_key);
  cluster_test!(streams, should_xgroup_create_no_mkstream);
  cluster_test!(streams, should_xgroup_create_mkstream);
  cluster_test!(streams, should_xgroup_createconsumer);
  cluster_test!(streams, should_xgroup_delconsumer);
  cluster_test!(streams, should_xgroup_destroy);
  cluster_test!(streams, should_xgroup_setid);
  cluster_test!(streams, should_xreadgroup_one_stream);
  cluster_test!(streams, should_xreadgroup_multiple_stream);
  cluster_test!(streams, should_xreadgroup_block);
  cluster_test!(streams, should_xack_one_id);
  cluster_test!(streams, should_xack_multiple_ids);
  cluster_test!(streams, should_xclaim_one_id);
  cluster_test!(streams, should_xclaim_multiple_ids);
  cluster_test!(streams, should_xclaim_with_justid);
  cluster_test!(streams, should_xautoclaim_default);
}

#[cfg(feature = "i-cluster")]
mod cluster {
  #[cfg(feature = "i-client")]
  cluster_test!(cluster, should_use_each_cluster_node);
}

#[cfg(feature = "i-tracking")]
mod tracking {
  #[cfg(feature = "i-keys")]
  cluster_test!(tracking, should_invalidate_foo_resp3);
}

#[cfg(feature = "i-time-series")]
mod timeseries {
  cluster_test!(timeseries, should_ts_add_get_and_range);
  cluster_test!(timeseries, should_create_alter_and_del_timeseries);
  cluster_test!(timeseries, should_madd_and_mget);
  cluster_test!(timeseries, should_incr_and_decr);
  cluster_test!(timeseries, should_create_and_delete_rules);
  cluster_test!(timeseries, should_madd_and_mrange);
  cluster_test!(timeseries, should_madd_and_mrevrange);
}

#[cfg(feature = "i-client")]
mod client {
  cluster_test!(client, should_echo_message);
}
