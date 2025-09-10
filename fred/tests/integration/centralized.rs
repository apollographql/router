#[cfg(feature = "i-keys")]
mod keys {
  centralized_test!(keys, should_handle_missing_keys);
  centralized_test!(keys, should_set_and_get_a_value);
  centralized_test!(keys, should_set_and_del_a_value);
  centralized_test!(keys, should_set_with_get_argument);
  centralized_test!(keys, should_setnx_value);
  centralized_test!(keys, should_incr_and_decr_a_value);
  centralized_test!(keys, should_incr_by_float);
  centralized_test!(keys, should_mset_a_non_empty_map);
  centralized_test_panic!(keys, should_error_mset_empty_map);
  centralized_test!(keys, should_expire_key);
  centralized_test!(keys, should_pexpire_key);
  centralized_test!(keys, should_persist_key);
  centralized_test!(keys, should_check_ttl);
  centralized_test!(keys, should_check_pttl);
  centralized_test!(keys, should_dump_key);
  centralized_test!(keys, should_dump_and_restore_key);
  centralized_test!(keys, should_modify_ranges);
  centralized_test!(keys, should_getset_value);
  centralized_test!(keys, should_getdel_value);
  centralized_test!(keys, should_get_strlen);
  centralized_test!(keys, should_mget_values);
  centralized_test!(keys, should_msetnx_values);
  centralized_test!(keys, should_copy_values);
  centralized_test!(keys, should_unlink);
  centralized_test_panic!(keys, should_error_rename_does_not_exist);
  centralized_test_panic!(keys, should_error_renamenx_does_not_exist);
  centralized_test!(keys, should_rename);
  centralized_test!(keys, should_renamenx);
  centralized_test!(keys, should_expire_time_value);
  centralized_test!(keys, should_pexpire_time_value);
  #[cfg(all(feature = "i-keys", feature = "i-hashes", feature = "i-sets"))]
  centralized_test!(keys, should_check_type_of_key);

  centralized_test!(keys, should_get_keys_from_pool_in_a_stream);
}

#[cfg(all(feature = "transactions", feature = "i-keys"))]
mod multi {
  centralized_test!(multi, should_run_get_set_trx);
  centralized_test_panic!(multi, should_run_error_get_set_trx);
}

mod other {
  centralized_test!(other, should_connect_correctly_via_init_interface);
  centralized_test!(other, should_fail_with_bad_host_via_init_interface);
  centralized_test!(other, should_connect_correctly_via_wait_interface);
  centralized_test!(other, should_fail_with_bad_host_via_wait_interface);
  centralized_test!(other, pool_should_connect_correctly_via_init_interface);
  centralized_test!(other, pool_should_fail_with_bad_host_via_init_interface);
  centralized_test!(other, pool_should_connect_correctly_via_wait_interface);
  centralized_test!(other, pool_should_fail_with_bad_host_via_wait_interface);
  centralized_test!(other, should_fail_on_centralized_connect);
  centralized_test!(other, should_safely_change_protocols_repeatedly);
  centralized_test!(other, should_gracefully_quit);
  centralized_test!(other, should_not_hang_on_concurrent_quit);

  #[cfg(feature = "i-pubsub")]
  centralized_test!(other, should_exit_event_task_with_error);
  #[cfg(all(feature = "transactions", feature = "i-keys", feature = "i-hashes"))]
  centralized_test!(other, should_fail_pipeline_transaction_error);
  #[cfg(all(feature = "transactions", feature = "i-keys"))]
  centralized_test!(other, should_pipeline_transaction);
  #[cfg(feature = "credential-provider")]
  centralized_test!(other, should_use_credential_provider);
  #[cfg(feature = "metrics")]
  centralized_test!(other, should_track_size_stats);
  #[cfg(all(feature = "i-client", feature = "i-lists"))]
  centralized_test!(other, should_automatically_unblock);
  #[cfg(all(feature = "i-client", feature = "i-lists"))]
  centralized_test!(other, should_manually_unblock);
  #[cfg(all(feature = "i-client", feature = "i-lists"))]
  centralized_test!(other, should_error_when_blocked);
  #[cfg(all(feature = "i-keys", feature = "i-hashes"))]
  centralized_test!(other, should_smoke_test_from_value_impl);
  #[cfg(feature = "i-keys")]
  centralized_test!(other, should_pipeline_all);
  #[cfg(all(feature = "i-keys", feature = "i-hashes"))]
  centralized_test!(other, should_pipeline_all_error_early);
  #[cfg(feature = "i-keys")]
  centralized_test!(other, should_pipeline_last);
  #[cfg(all(feature = "i-keys", feature = "i-hashes"))]
  centralized_test!(other, should_pipeline_try_all);
  #[cfg(feature = "i-server")]
  centralized_test!(other, should_use_all_cluster_nodes_repeatedly);
  #[cfg(feature = "i-lists")]
  centralized_test!(other, should_support_options_with_pipeline);
  #[cfg(feature = "i-keys")]
  centralized_test!(other, should_reuse_pipeline);
  #[cfg(all(feature = "i-keys", feature = "i-lists"))]
  centralized_test!(other, should_manually_connect_twice);
  #[cfg(all(feature = "transactions", feature = "i-keys"))]
  centralized_test!(other, should_mix_trx_and_get);
  #[cfg(all(feature = "transactions", feature = "i-keys"))]
  centralized_test!(other, should_support_options_with_trx);

  //#[cfg(feature = "dns")]
  // centralized_test!(other, should_use_trust_dns);
  // centralized_test!(other, should_test_high_concurrency_pool);

  #[cfg(all(feature = "partial-tracing", feature = "i-keys"))]
  centralized_test!(other, should_use_tracing_get_set);
  #[cfg(feature = "subscriber-client")]
  centralized_test!(other, should_ping_with_subscriber_client);

  #[cfg(all(feature = "replicas", feature = "i-keys"))]
  centralized_test!(other, should_replica_set_and_get);
  #[cfg(all(feature = "replicas", feature = "i-keys"))]
  centralized_test!(other, should_replica_set_and_get_not_lazy);
  #[cfg(all(feature = "replicas", feature = "i-keys"))]
  centralized_test!(other, should_pipeline_with_replicas);
}

mod pool {
  centralized_test!(pool, should_connect_and_ping_static_pool_single_conn);
  centralized_test!(pool, should_connect_and_ping_static_pool_two_conn);
  #[cfg(feature = "i-keys")]
  centralized_test!(pool, should_incr_exclusive_pool);
  #[cfg(all(feature = "i-keys", feature = "transactions"))]
  centralized_test!(pool, should_watch_and_trx_exclusive_pool);
}

#[cfg(feature = "i-hashes")]
mod hashes {
  centralized_test!(hashes, should_hset_and_hget);
  centralized_test!(hashes, should_hset_and_hdel);
  centralized_test!(hashes, should_hexists);
  centralized_test!(hashes, should_hgetall);
  centralized_test!(hashes, should_hincryby);
  centralized_test!(hashes, should_hincryby_float);
  centralized_test!(hashes, should_get_keys);
  centralized_test!(hashes, should_hmset);
  centralized_test!(hashes, should_hmget);
  centralized_test!(hashes, should_hsetnx);
  centralized_test!(hashes, should_get_random_field);
  centralized_test!(hashes, should_get_strlen);
  centralized_test!(hashes, should_get_values);
  #[cfg(feature = "i-hexpire")]
  centralized_test!(hashes, should_do_hash_expirations);
  #[cfg(feature = "i-hexpire")]
  centralized_test!(hashes, should_do_hash_pexpirations);
}

#[cfg(feature = "i-pubsub")]
mod pubsub {
  centralized_test!(pubsub, should_publish_and_recv_messages);
  centralized_test!(pubsub, should_psubscribe_and_recv_messages);
  centralized_test!(pubsub, should_unsubscribe_from_all);

  centralized_test!(pubsub, should_get_pubsub_channels);
  centralized_test!(pubsub, should_get_pubsub_numpat);
  centralized_test!(pubsub, should_get_pubsub_nunmsub);
  centralized_test!(pubsub, should_get_pubsub_shard_channels);
  centralized_test!(pubsub, should_get_pubsub_shard_numsub);
}

#[cfg(feature = "i-hyperloglog")]
mod hyperloglog {
  centralized_test!(hyperloglog, should_pfadd_elements);
  centralized_test!(hyperloglog, should_pfcount_elements);
  centralized_test!(hyperloglog, should_pfmerge_elements);
}

mod scanning {
  #[cfg(feature = "i-keys")]
  centralized_test!(scanning, should_scan_keyspace);
  #[cfg(feature = "i-hashes")]
  centralized_test!(scanning, should_hscan_hash);
  #[cfg(feature = "i-sets")]
  centralized_test!(scanning, should_sscan_set);
  #[cfg(feature = "i-sorted-sets")]
  centralized_test!(scanning, should_zscan_sorted_set);
  #[cfg(feature = "i-keys")]
  centralized_test!(scanning, should_scan_buffered);
  #[cfg(feature = "i-keys")]
  centralized_test!(scanning, should_continue_scanning_on_page_drop);
  #[cfg(feature = "i-keys")]
  centralized_test!(scanning, should_scan_by_page_centralized);
}

#[cfg(feature = "i-slowlog")]
mod slowlog {
  centralized_test!(slowlog, should_read_slowlog_length);
  centralized_test!(slowlog, should_read_slowlog_entries);
  centralized_test!(slowlog, should_reset_slowlog);
}

#[cfg(feature = "i-server")]
mod server {
  centralized_test!(server, should_flushall);
  centralized_test!(server, should_read_server_info);
  centralized_test!(server, should_ping_pong_command);
  centralized_test!(server, should_read_last_save);
  centralized_test!(server, should_read_db_size);
  centralized_test!(server, should_start_bgsave);
  centralized_test!(server, should_do_bgrewriteaof);
  centralized_test!(server, should_select_index_command);
}

#[cfg(feature = "i-sets")]
mod sets {
  centralized_test!(sets, should_sadd_elements);
  centralized_test!(sets, should_scard_elements);
  centralized_test!(sets, should_sdiff_elements);
  centralized_test!(sets, should_sdiffstore_elements);
  centralized_test!(sets, should_sinter_elements);
  centralized_test!(sets, should_sinterstore_elements);
  centralized_test!(sets, should_check_sismember);
  centralized_test!(sets, should_check_smismember);
  centralized_test!(sets, should_read_smembers);
  centralized_test!(sets, should_smove_elements);
  centralized_test!(sets, should_spop_elements);
  centralized_test!(sets, should_get_random_member);
  centralized_test!(sets, should_remove_elements);
  centralized_test!(sets, should_sunion_elements);
  centralized_test!(sets, should_sunionstore_elements);
}

#[cfg(feature = "i-memory")]
pub mod memory {
  centralized_test!(memory, should_run_memory_doctor);
  centralized_test!(memory, should_run_memory_malloc_stats);
  centralized_test!(memory, should_run_memory_purge);
  centralized_test!(memory, should_run_memory_stats);
  centralized_test!(memory, should_run_memory_usage);
}

#[cfg(feature = "i-scripts")]
pub mod lua {
  #[cfg(feature = "sha-1")]
  centralized_test!(lua, should_load_script);
  centralized_test!(lua, should_eval_echo_script);
  #[cfg(feature = "sha-1")]
  centralized_test!(lua, should_eval_get_script);
  #[cfg(feature = "sha-1")]
  centralized_test!(lua, should_evalsha_echo_script);
  #[cfg(feature = "sha-1")]
  centralized_test!(lua, should_evalsha_with_reload_echo_script);
  #[cfg(feature = "sha-1")]
  centralized_test!(lua, should_evalsha_get_script);

  centralized_test!(lua, should_function_load_scripts);
  centralized_test!(lua, should_function_dump_and_restore);
  centralized_test!(lua, should_function_flush);
  centralized_test!(lua, should_function_delete);
  centralized_test!(lua, should_function_list);
  centralized_test!(lua, should_function_list_multiple);
  #[cfg(feature = "i-keys")]
  centralized_test!(lua, should_function_fcall_getset);
  centralized_test!(lua, should_function_fcall_echo);
  centralized_test!(lua, should_function_fcall_ro_echo);

  #[cfg(feature = "sha-1")]
  centralized_test!(lua, should_create_lua_script_helper_from_code);
  #[cfg(feature = "sha-1")]
  centralized_test!(lua, should_create_lua_script_helper_from_hash);
  centralized_test!(lua, should_create_function_from_code);
  centralized_test!(lua, should_create_function_from_name);
}

#[cfg(feature = "i-sorted-sets")]
pub mod sorted_sets {
  centralized_test!(sorted_sets, should_bzpopmin);
  centralized_test!(sorted_sets, should_bzpopmax);
  centralized_test!(sorted_sets, should_zadd_values);
  centralized_test!(sorted_sets, should_zcard_values);
  centralized_test!(sorted_sets, should_zcount_values);
  centralized_test!(sorted_sets, should_zdiff_values);
  centralized_test!(sorted_sets, should_zdiffstore_values);
  centralized_test!(sorted_sets, should_zincrby_values);
  centralized_test!(sorted_sets, should_zinter_values);
  centralized_test!(sorted_sets, should_zinterstore_values);
  centralized_test!(sorted_sets, should_zlexcount);
  centralized_test!(sorted_sets, should_zpopmax);
  centralized_test!(sorted_sets, should_zpopmin);
  centralized_test!(sorted_sets, should_zrandmember);
  centralized_test!(sorted_sets, should_zrangestore_values);
  centralized_test!(sorted_sets, should_zrangebylex);
  centralized_test!(sorted_sets, should_zrevrangebylex);
  centralized_test!(sorted_sets, should_zrangebyscore);
  centralized_test!(sorted_sets, should_zrevrangebyscore);
  centralized_test!(sorted_sets, should_zrank_values);
  centralized_test!(sorted_sets, should_zrank_values_withscore);
  centralized_test!(sorted_sets, should_zrem_values);
  centralized_test!(sorted_sets, should_zremrangebylex);
  centralized_test!(sorted_sets, should_zremrangebyrank);
  centralized_test!(sorted_sets, should_zremrangebyscore);
  centralized_test!(sorted_sets, should_zrevrank_values);
  centralized_test!(sorted_sets, should_zscore_values);
  centralized_test!(sorted_sets, should_zunion_values);
  centralized_test!(sorted_sets, should_zunionstore_values);
  centralized_test!(sorted_sets, should_zmscore_values);
  centralized_test!(sorted_sets, should_zrangebyscore_neg_infinity);
}

#[cfg(feature = "i-lists")]
pub mod lists {
  centralized_test!(lists, should_blpop_values);
  centralized_test!(lists, should_brpop_values);
  centralized_test!(lists, should_brpoplpush_values);
  centralized_test!(lists, should_blmove_values);

  centralized_test!(lists, should_lindex_values);
  centralized_test!(lists, should_linsert_values);
  centralized_test!(lists, should_lpop_values);
  centralized_test!(lists, should_lpos_values);
  centralized_test!(lists, should_lpush_values);
  centralized_test!(lists, should_lpushx_values);
  centralized_test!(lists, should_lrange_values);
  centralized_test!(lists, should_lrem_values);
  centralized_test!(lists, should_lset_values);
  #[cfg(feature = "i-keys")]
  centralized_test!(lists, should_ltrim_values);
  centralized_test!(lists, should_rpop_values);
  centralized_test!(lists, should_rpoplpush_values);
  centralized_test!(lists, should_lmove_values);
  centralized_test!(lists, should_rpush_values);
  centralized_test!(lists, should_rpushx_values);
  centralized_test!(lists, should_sort_int_list);
  centralized_test!(lists, should_sort_alpha_list);
  centralized_test!(lists, should_sort_int_list_with_limit);
  #[cfg(feature = "i-keys")]
  centralized_test!(lists, should_sort_int_list_with_patterns);
}

#[cfg(feature = "i-geo")]
pub mod geo {
  centralized_test!(geo, should_geoadd_values);
  centralized_test!(geo, should_geohash_values);
  centralized_test!(geo, should_geopos_values);
  centralized_test!(geo, should_geodist_values);
  centralized_test!(geo, should_georadius_values);
  centralized_test!(geo, should_georadiusbymember_values);
  centralized_test!(geo, should_geosearch_values);
}

#[cfg(feature = "i-acl")]
pub mod acl {
  centralized_test!(acl, should_auth_as_test_user);
  centralized_test!(acl, should_auth_as_test_user_via_config);
  centralized_test!(acl, should_run_acl_getuser);
}

#[cfg(feature = "i-streams")]
mod streams {
  centralized_test!(streams, should_xinfo_consumers);
  centralized_test!(streams, should_xinfo_groups);
  centralized_test!(streams, should_xinfo_streams);
  centralized_test!(streams, should_xadd_auto_id_to_a_stream);
  centralized_test!(streams, should_xadd_manual_id_to_a_stream);
  centralized_test!(streams, should_xadd_with_cap_to_a_stream);
  centralized_test!(streams, should_xadd_nomkstream_to_a_stream);
  centralized_test!(streams, should_xtrim_a_stream_approx_cap);
  centralized_test!(streams, should_xtrim_a_stream_eq_cap);
  centralized_test!(streams, should_xdel_one_id_in_a_stream);
  centralized_test!(streams, should_xdel_multiple_ids_in_a_stream);
  centralized_test!(streams, should_xrange_no_count);
  centralized_test!(streams, should_xrange_with_count);
  centralized_test!(streams, should_xrange_values_no_count);
  centralized_test!(streams, should_xrevrange_no_count);
  centralized_test!(streams, should_xrevrange_with_count);
  centralized_test!(streams, should_xrevrange_values_no_count);
  centralized_test!(streams, should_run_xlen_on_stream);
  centralized_test!(streams, should_xread_one_key_count_1);
  centralized_test!(streams, should_xread_map_one_key);
  centralized_test!(streams, should_xread_multiple_keys_count_2);
  centralized_test!(streams, should_xread_with_blocking);
  centralized_test!(streams, should_xgroup_create_no_mkstream);
  centralized_test!(streams, should_xgroup_create_mkstream);
  centralized_test!(streams, should_xgroup_createconsumer);
  centralized_test!(streams, should_xgroup_delconsumer);
  centralized_test!(streams, should_xgroup_destroy);
  centralized_test!(streams, should_xgroup_setid);
  centralized_test!(streams, should_xreadgroup_one_stream);
  centralized_test!(streams, should_xreadgroup_multiple_stream);
  centralized_test!(streams, should_xreadgroup_block);
  centralized_test!(streams, should_xack_one_id);
  centralized_test!(streams, should_xack_multiple_ids);
  centralized_test!(streams, should_xclaim_one_id);
  centralized_test!(streams, should_xclaim_multiple_ids);
  centralized_test!(streams, should_xclaim_with_justid);
  centralized_test!(streams, should_xautoclaim_default);
}

#[cfg(feature = "i-tracking")]
mod tracking {
  #[cfg(feature = "i-keys")]
  centralized_test!(tracking, should_invalidate_foo_resp3);
  #[cfg(feature = "i-keys")]
  centralized_test!(tracking, should_invalidate_foo_resp2_centralized);
}

// The CI settings for redis-stack only support centralized configs for now.
#[cfg(feature = "i-redis-json")]
mod redis_json {
  centralized_test!(redis_json, should_get_and_set_basic_obj);
  centralized_test!(redis_json, should_get_and_set_stringified_obj);
  centralized_test!(redis_json, should_array_append);
  centralized_test!(redis_json, should_modify_arrays);
  centralized_test!(redis_json, should_pop_and_trim_arrays);
  centralized_test!(redis_json, should_get_set_del_obj);
  centralized_test!(redis_json, should_merge_objects);
  centralized_test!(redis_json, should_mset_and_mget);
  centralized_test!(redis_json, should_incr_numbers);
  centralized_test!(redis_json, should_inspect_objects);
  centralized_test!(redis_json, should_modify_strings);
  centralized_test!(redis_json, should_toggle_boolean);
  centralized_test!(redis_json, should_get_value_type);
}

#[cfg(feature = "i-time-series")]
mod timeseries {
  centralized_test!(timeseries, should_ts_add_get_and_range);
  centralized_test!(timeseries, should_create_alter_and_del_timeseries);
  centralized_test!(timeseries, should_madd_and_mget);
  centralized_test!(timeseries, should_incr_and_decr);
  centralized_test!(timeseries, should_create_and_delete_rules);
  centralized_test!(timeseries, should_madd_and_mrange);
  centralized_test!(timeseries, should_madd_and_mrevrange);
}

#[cfg(feature = "i-redisearch")]
mod redisearch {
  centralized_test!(redisearch, should_list_indexes);
  centralized_test!(redisearch, should_index_and_info_basic_hash);
  centralized_test!(redisearch, should_index_and_search_hash);
  centralized_test!(redisearch, should_index_and_aggregate_timestamps);
}

#[cfg(feature = "i-client")]
mod client {
  centralized_test!(client, should_echo_message);
}
