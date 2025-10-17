-- Add down migration script here
--
SELECT
    cron.unschedule ('delete-old-cache-entries');

DROP EXTENSION IF EXISTS pg_cron;

DROP TABLE "invalidation_key";

DROP TABLE "cache";
