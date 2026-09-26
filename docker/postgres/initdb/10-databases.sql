-- The yen-locale database the money live test connects to. `lc_monetary`
-- is set per-database (rather than server-wide) so the `app` database keeps
-- the ordinary two-digit locale for contrast in the same test.
-- (`lc_monetary` is not a CREATE DATABASE option, hence the ALTER.)
CREATE DATABASE zippa_yen TEMPLATE template0;
ALTER DATABASE zippa_yen SET lc_monetary = 'ja_JP.utf8';

-- Loaded via shared_preload_libraries in compose.yaml; creating it here lets
-- the query-digest live test exercise its "available" branch. Runs in the
-- default (app) database, which is what that test connects to.
CREATE EXTENSION IF NOT EXISTS pg_stat_statements;
