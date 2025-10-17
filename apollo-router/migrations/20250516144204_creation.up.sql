-- Add up migration script here
-- Add migration script here
CREATE EXTENSION IF NOT EXISTS pg_cron;
CREATE OR REPLACE FUNCTION create_index(table_name text, index_name text, column_name text) RETURNS void AS $$
declare
   l_count integer;
begin
  select count(*)
     into l_count
  from pg_indexes
  where schemaname = 'public'
    and tablename = lower(table_name)
    and indexname = lower(index_name);

  if l_count = 0 then
     execute 'create index ' || index_name || ' on "' || table_name || '"(' || column_name || ')';
  end if;
end;
$$ LANGUAGE plpgsql;

CREATE OR REPLACE FUNCTION create_unique_index(table_name text, index_name text, column_names text) RETURNS void AS $$
declare
   l_count integer;
begin
  select count(*)
     into l_count
  from pg_indexes
  where schemaname = 'public'
    and tablename = lower(table_name)
    and indexname = lower(index_name);

  if l_count = 0 then
     execute 'create unique index ' || index_name || ' on "' || table_name || '"(' || array_to_string(string_to_array(column_names, ',') , ',') || ')';
  end if;
end;
$$ LANGUAGE plpgsql;

CREATE OR REPLACE FUNCTION create_foreign_key(fk_name text, table_name_child text, table_name_parent text, column_name_child text, column_name_parent text) RETURNS void AS $$
declare
   l_count integer;
begin
  select count(*)
     into l_count
  from information_schema.table_constraints as tc
  where constraint_type = 'FOREIGN KEY'
    and tc.table_name = lower(table_name_child)
    and tc.constraint_name = lower(fk_name);

  if l_count = 0 then
     execute 'alter table "' || table_name_child || '" ADD CONSTRAINT ' || fk_name || ' FOREIGN KEY(' || column_name_child || ') REFERENCES "' || table_name_parent || '"(' || column_name_parent || ')';
  end if;
end;
$$ LANGUAGE plpgsql;


CREATE UNLOGGED TABLE IF NOT EXISTS "invalidation_key" (cache_key_id BIGSERIAL NOT NULL, invalidation_key VARCHAR(255) NOT NULL, subgraph_name VARCHAR(255) NOT NULL, PRIMARY KEY(cache_key_id, invalidation_key, subgraph_name));
CREATE UNLOGGED TABLE IF NOT EXISTS "cache" (id BIGSERIAL PRIMARY KEY, cache_key VARCHAR(1024) NOT NULL, data TEXT NOT NULL, control TEXT NOT NULL, expires_at TIMESTAMP WITH TIME ZONE NOT NULL);

ALTER TABLE invalidation_key ADD CONSTRAINT FK_INVALIDATION_KEY_CACHE FOREIGN KEY (cache_key_id) references cache (id) ON delete cascade;
SELECT create_unique_index('cache', 'cache_key_idx', 'cache_key');

-- Remove expired data every hour
SELECT cron.schedule('delete-old-cache-entries', '0 * * * *', $$
    DELETE FROM cache
    WHERE expires_at < NOW()
$$);
