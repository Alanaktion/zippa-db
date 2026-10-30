//! Tests against real MySQL/Postgres servers (`#[ignore]`d); see `script/test-db`.

use super::*;

/// A `SELECT` that writes through a function is refused on the agent's
/// read-only session. Against `compose.yaml`'s `postgres` service.
#[tokio::test]
#[ignore = "needs a live Postgres server; see the doc comment"]
async fn live_postgres_an_agent_read_cannot_write_through_a_function() {
    let connection = live_postgres("ZIPPA_TEST_POSTGRES_URL", "app").await;
    connection
        .execute("DROP SEQUENCE IF EXISTS zippa_agent_probe", vec![])
        .await
        .expect("could not drop the probe sequence");
    connection
        .execute("CREATE SEQUENCE zippa_agent_probe", vec![])
        .await
        .expect("could not create the probe sequence");

    let refused = connection
        .run_query_as_agent("SELECT nextval('zippa_agent_probe')", 100)
        .await;
    assert!(refused.is_err(), "nextval should be refused read-only");
    let read = connection
        .run_query_as_agent("SELECT last_value, is_called FROM zippa_agent_probe", 100)
        .await
        .expect("reading the sequence should run");
    assert_eq!(read.rows[0][1].as_deref(), Some("0"));

    connection
        .execute("DROP SEQUENCE zippa_agent_probe", vec![])
        .await
        .expect("could not drop the probe sequence");
    connection.close().await;
}

/// An agent read past its row cap stops reading, without draining the rest,
/// and the pool answers normally afterwards. Against `compose.yaml`'s
/// `postgres` service.
#[tokio::test]
#[ignore = "needs a live Postgres server; see the doc comment"]
async fn live_postgres_an_agent_read_stops_at_its_cap() {
    let connection = live_postgres("ZIPPA_TEST_POSTGRES_URL", "app").await;
    let read = connection
        .run_query_as_agent("SELECT g FROM generate_series(1, 5000000) AS g", 101)
        .await
        .expect("a capped read should run");
    assert_eq!(read.rows.len(), 101);
    assert_eq!(read.rows[100][0].as_deref(), Some("101"));
    let after = connection
        .run_query("SELECT 1")
        .await
        .expect("the pool should still answer");
    assert_eq!(after.rows[0][0].as_deref(), Some("1"));
    connection.close().await;
}

/// The same round trip on a live Postgres server, where the decode is
/// `decode($1, 'hex')` and the key is cast the way `update_statement` casts
/// it.
///
/// Ignored by default because it needs a server; see
/// `live_postgres_money_scales_by_the_servers_locale_not_always_by_100` for
/// how to start one. `cargo test -- --ignored live_postgres_binary`.
#[tokio::test]
#[ignore = "needs a live Postgres server; see the doc comment"]
async fn live_postgres_binary_value_round_trips_through_decode() {
    let connection = live_postgres("ZIPPA_TEST_POSTGRES_URL", "app").await;

    connection
        .execute("DROP TABLE IF EXISTS zippa_binary_probe", vec![])
        .await
        .expect("could not drop the probe table");
    connection
        .execute(
            "CREATE TABLE zippa_binary_probe (id INT PRIMARY KEY, payload BYTEA)",
            vec![],
        )
        .await
        .expect("could not create the probe table");
    connection
        .execute(
            "INSERT INTO zippa_binary_probe VALUES (1, decode('001122', 'hex'))",
            vec![],
        )
        .await
        .expect("could not seed the probe table");

    let sql = format!(
        "UPDATE zippa_binary_probe SET payload = {} WHERE id = {}",
        binary_placeholder(Engine::Postgres, 1),
        typed_placeholder(Engine::Postgres, 2, "INT4")
    );
    let rows = connection
        .execute(
            &sql,
            vec![Some("deadbeef".to_string()), Some("1".to_string())],
        )
        .await
        .expect("the update failed");
    assert_eq!(rows, 1);

    let bytes = connection
        .fetch_binary(
            "SELECT payload FROM zippa_binary_probe WHERE id = cast($1 as INT4)",
            vec![Some("1".to_string())],
        )
        .await
        .expect("fetch failed");
    assert_eq!(bytes, Some(vec![0xde, 0xad, 0xbe, 0xef]));

    connection
        .execute("DROP TABLE zippa_binary_probe", vec![])
        .await
        .expect("could not drop the probe table");
    connection.close().await;
}

/// `money` scales by `lc_monetary`'s fraction-digit count, which is two in
/// most locales but zero for a currency like the yen — so the raw integer a
/// `money` column decodes to means a different amount depending on the
/// server's locale, and a client that always divides by 100 would silently
/// show the wrong number for the others. This is the live half of
/// `postgres::money_keeps_both_digits_at_the_common_scale` and its sibling
/// scale tests: that `Connection::open`'s probe actually reads a real
/// server's scale rather than assuming it.
///
/// Ignored by default because it needs a server. Start one and run it:
///
/// ```text
/// docker run --rm -d -p 5433:5432 --name zippa-postgres \
///   -e POSTGRES_PASSWORD=secret -e POSTGRES_DB=app postgres:16
/// docker exec zippa-postgres psql -U postgres -c "CREATE DATABASE zippa_yen"
/// docker exec zippa-postgres psql -U postgres -d zippa_yen \
///   -c "ALTER DATABASE zippa_yen SET lc_monetary = 'ja_JP.utf8'"
/// cargo test -- --ignored live_postgres
/// ```
///
/// The yen locale needs generating first if the image does not already carry
/// it (`locale-gen ja_JP.UTF-8` inside the container, then a restart) —
/// confirmed against a plain `postgres:16` image, which does not.
///
/// `ZIPPA_TEST_POSTGRES_YEN_URL` (default
/// `postgres://postgres:secret@127.0.0.1:5433/zippa_yen`) points the test at
/// another server; `ZIPPA_TEST_POSTGRES_URL` (default
/// `postgres://postgres:secret@127.0.0.1:5433/app`) is the same server's
/// ordinary, two-digit database, for contrast.
#[tokio::test]
#[ignore = "needs a live Postgres server with a yen-locale database; see the doc comment"]
async fn live_postgres_money_scales_by_the_servers_locale_not_always_by_100() {
    let usual = live_postgres("ZIPPA_TEST_POSTGRES_URL", "app").await;
    let result = usual
        .run_query("SELECT '12.34'::money")
        .await
        .expect("could not read the ordinary-locale money value");
    assert_eq!(
        result.rows[0][0].as_deref(),
        Some("12.34"),
        "a two-digit locale should read back the way it was written"
    );
    usual.close().await;

    let yen = live_postgres("ZIPPA_TEST_POSTGRES_YEN_URL", "zippa_yen").await;
    let result = yen
        .run_query("SELECT '1234'::money")
        .await
        .expect("could not read the yen-locale money value");
    assert_eq!(
        result.rows[0][0].as_deref(),
        Some("1234"),
        "a zero-digit locale's value should not be shown divided by 100"
    );
    yen.close().await;
}

/// The SSL mode reaches the server: the compose Postgres runs without SSL, so
/// `Require` must be refused rather than quietly falling back to a plain
/// connection, while `Disable` and the default `Prefer` still connect.
#[tokio::test]
#[ignore = "needs a live Postgres server; see the doc comment"]
async fn live_postgres_ssl_require_is_refused_by_a_server_without_ssl() {
    for mode in [SslMode::Disable, SslMode::Prefer] {
        let (mut config, password) = live_postgres_config("ZIPPA_TEST_POSTGRES_URL", "app");
        config.ssl.mode = mode;
        let connection = Connection::open(config, Some(password))
            .await
            .unwrap_or_else(|error| panic!("{mode:?} should connect: {error:#}"));
        connection.close().await;
    }

    let (mut config, password) = live_postgres_config("ZIPPA_TEST_POSTGRES_URL", "app");
    config.ssl.mode = SslMode::Require;
    assert!(
        Connection::open(config, Some(password)).await.is_err(),
        "Require should refuse a server that does not offer SSL"
    );
}

/// A connection through an SSH tunnel reaches Postgres, keeps the host the
/// user saved rather than the tunnel's end, and switching database reuses the
/// tunnel rather than signing in again.
///
/// Needs the compose `ssh` service besides `postgres`; see `live_ssh`.
#[tokio::test]
#[ignore = "needs a live Postgres server and SSH jump host; see the doc comment"]
async fn live_postgres_connects_through_an_ssh_tunnel() {
    let (ssh, ssh_password, target_host, target_port) = live_ssh();
    let (mut config, password) = live_postgres_config("ZIPPA_TEST_POSTGRES_URL", "app");
    config.host = target_host.clone();
    config.port = target_port;
    config.ssh = ssh;

    let connection = Connection::open_with(
        config,
        Credentials {
            password: Some(password),
            ssh: Some(ssh_password),
        },
    )
    .await
    .expect("should connect through the tunnel");
    let result = connection
        .run_query("SELECT 1 + 1")
        .await
        .expect("should query through the tunnel");
    assert_eq!(result.rows[0][0].as_deref(), Some("2"));
    assert_eq!(connection.config.host, target_host);

    let other = connection
        .with_database("postgres")
        .await
        .expect("switching database should reuse the tunnel");
    other
        .run_query_with("SELECT 1", Vec::new())
        .await
        .expect("query");
    other.close().await;
    connection.close().await;
}

/// The types `postgres::cell` decodes field by field — geometry, ranges,
/// `hstore`, `macaddr8` — read back as the text Postgres itself prints, and
/// that text casts back to the same value, which is what makes them
/// editable rather than `<POINT>`-style stand-ins.
///
/// Ignored by default because it needs a server; see
/// `live_postgres_money_scales_by_the_servers_locale_not_always_by_100` for
/// how to start one. `cargo test -- --ignored live_postgres_geometry`.
#[tokio::test]
#[ignore = "needs a live Postgres server; see the doc comment"]
async fn live_postgres_geometry_ranges_hstore_and_macaddr8_read_back_as_text() {
    let connection = live_postgres("ZIPPA_TEST_POSTGRES_URL", "app").await;
    connection
        .execute("CREATE EXTENSION IF NOT EXISTS hstore", vec![])
        .await
        .expect("could not install hstore");

    let cases = [
        ("'(1.5,-2)'::point", "(1.5,-2)"),
        ("'{1,-1,0}'::line", "{1,-1,0}"),
        ("'[(0,0),(1,1)]'::lseg", "[(0,0),(1,1)]"),
        ("'((0,0),(2,2))'::box", "(2,2),(0,0)"),
        ("'((0,0),(1,1),(2,0))'::path", "((0,0),(1,1),(2,0))"),
        ("'[(0,0),(1,1)]'::path", "[(0,0),(1,1)]"),
        ("'((0,0),(1,1),(2,0))'::polygon", "((0,0),(1,1),(2,0))"),
        ("'<(1,2),3>'::circle", "<(1,2),3>"),
        ("ARRAY['(1,2)'::point, NULL]", "{\"(1,2)\",NULL}"),
        ("'[1,10)'::int4range", "[1,10)"),
        ("'(,5]'::int8range", "(,6)"),
        ("'empty'::int4range", "empty"),
        ("'1.5'::numeric", "1.5"),
        ("'1.50'::numeric", "1.50"),
        ("'[1.50,)'::numrange", "[1.50,)"),
        ("'[1.5,2.5]'::numrange", "[1.5,2.5]"),
        (
            "'[2024-01-01,2024-02-01)'::daterange",
            "[2024-01-01,2024-02-01)",
        ),
        (
            "'[2024-01-01 10:00,2024-01-02)'::tsrange",
            "[\"2024-01-01 10:00:00\",\"2024-01-02 00:00:00\")",
        ),
        (
            "'a=>1, \"b c\"=>NULL, q=>\"say \\\"hi\\\"\"'::hstore",
            "\"a\"=>\"1\", \"b c\"=>NULL, \"q\"=>\"say \\\"hi\\\"\"",
        ),
        (
            "'08:00:2b:01:02:03:04:05'::macaddr8",
            "08:00:2b:01:02:03:04:05",
        ),
    ];
    for (expression, expected) in cases {
        let result = connection
            .run_query(&format!("SELECT {expression}"))
            .await
            .unwrap_or_else(|error| panic!("could not read {expression}: {error}"));
        let shown = result.rows[0][0].clone();
        assert_eq!(shown.as_deref(), Some(expected), "{expression}");

        // What the grid shows is what an edit binds back, through the same
        // cast `typed_placeholder` writes, and it should come out as the
        // value it was read from.
        let type_name = &result.column_types[0];
        let round_trip = connection
            .run_query_with(
                &format!(
                    "SELECT {}::text, ({expression})::text",
                    crate::db::sql::typed_placeholder(Engine::Postgres, 1, type_name)
                ),
                vec![shown],
            )
            .await
            .unwrap_or_else(|error| panic!("could not cast {expected} back: {error}"));
        assert_eq!(
            round_trip.rows[0][0], round_trip.rows[0][1],
            "{expected} should read back as {expression}"
        );
    }

    // `tstzrange` is shown in the local zone, so only its shape is pinned:
    // west of UTC the bound is still the evening of 2023-12-31.
    let result = connection
        .run_query("SELECT '[2024-01-01 00:00+00,)'::tstzrange")
        .await
        .expect("could not read a tstzrange");
    let shown = result.rows[0][0].clone().unwrap_or_default();
    assert!(
        (shown.starts_with("[\"2024-01-01") || shown.starts_with("[\"2023-12-31"))
            && shown.ends_with("\",)"),
        "{shown}"
    );

    connection.close().await;
}

/// `processes` reads every other connection's activity and `kill_process` can
/// end one of them — the live half of the process list, since SQLite (the
/// only engine the rest of this file exercises) has no server activity to
/// read.
///
/// Ignored by default because it needs a server; see
/// `live_postgres_money_scales_by_the_servers_locale_not_always_by_100` for
/// how to start one. `cargo test -- --ignored live_postgres_processes`.
#[tokio::test]
#[ignore = "needs a live Postgres server; see the doc comment"]
async fn live_postgres_processes_lists_and_kills_another_connection() {
    let watcher = live_postgres("ZIPPA_TEST_POSTGRES_URL", "app").await;
    let victim = live_postgres("ZIPPA_TEST_POSTGRES_URL", "app").await;

    let sleeper = tokio::spawn(async move { victim.run_query("SELECT pg_sleep(30)").await });
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    let processes = watcher
        .processes()
        .await
        .expect("could not read the process list");
    let row = processes
        .rows
        .iter()
        .find(|row| {
            row.get(6)
                .and_then(|cell| cell.as_deref())
                .is_some_and(|query| query.contains("pg_sleep"))
        })
        .expect("the sleeping connection should be in the process list");
    let pid = row[0].clone().expect("a pid");

    watcher
        .kill_process(&pid)
        .await
        .expect("could not end the sleeping connection");

    let result = sleeper.await.expect("the task itself should not panic");
    assert!(
        result.is_err(),
        "the killed connection's query should have been interrupted"
    );

    watcher.close().await;
}

/// A statement timeout is set on every connection the pool opens, and the
/// server cancels a statement that runs past it.
#[tokio::test]
#[ignore = "needs a live Postgres server; see live_postgres_processes_lists_and_kills_another_connection"]
async fn live_postgres_statement_timeout_cancels_a_long_statement() {
    let connection = live_postgres_with("ZIPPA_TEST_POSTGRES_URL", "app", |config| {
        config.statement_timeout = Some(1);
    })
    .await;

    let shown = connection
        .run_query("SHOW statement_timeout")
        .await
        .expect("could not read the setting");
    assert_eq!(shown.rows[0][0].as_deref(), Some("1s"));

    // Not `pg_sleep`: the process-list test running alongside ends any
    // backend whose query mentions it, which would read as a lost connection
    // rather than a timeout.
    let error = connection
        .run_query("SELECT count(*) FROM generate_series(1, 10000000000) AS zippa_timeout_test")
        .await
        .expect_err("the server should cancel a statement past the timeout");
    assert!(
        format!("{error:#}").contains("statement timeout"),
        "{error:#}"
    );

    connection.close().await;
}

/// A backend the server ends mid-statement reads as a lost connection in
/// words, not as the driver's own text.
#[tokio::test]
#[ignore = "needs a live Postgres server; see live_postgres_processes_lists_and_kills_another_connection"]
async fn live_postgres_a_terminated_backend_reads_as_a_lost_connection() {
    let watcher = live_postgres("ZIPPA_TEST_POSTGRES_URL", "app").await;
    let victim = Arc::new(live_postgres("ZIPPA_TEST_POSTGRES_URL", "app").await);

    let sleeper = {
        let victim = victim.clone();
        // Named so the query below ends this backend alone, and without
        // `pg_sleep` in it so the process-list test alongside does not end
        // it first.
        tokio::spawn(async move {
            victim
                .run_query(
                    "SELECT count(*) FROM generate_series(1, 10000000000) AS zippa_terminate_test",
                )
                .await
        })
    };
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    watcher
        .run_query(
            "SELECT pg_terminate_backend(pid) FROM pg_stat_activity \
             WHERE query LIKE '%zippa_terminate_test%' AND pid <> pg_backend_pid()",
        )
        .await
        .expect("could not end the sleeping backend");

    let error = sleeper
        .await
        .expect("the task itself should not panic")
        .expect_err("the ended backend's query should fail");
    assert_eq!(
        crate::db::health::ConnectionTrouble::of(&error),
        Some(crate::db::health::ConnectionTrouble::Lost),
        "{error:#}"
    );

    watcher.close().await;
    victim.close().await;
}

/// `server_variables` reads `pg_settings`, with `max_connections` (a
/// `postmaster`-context setting every server has) standing in for "the list
/// actually came back shaped the way `VARIABLES_SQL` assumes."
#[tokio::test]
#[ignore = "needs a live Postgres server; see live_postgres_processes_lists_and_kills_another_connection"]
async fn live_postgres_server_variables_lists_max_connections() {
    let connection = live_postgres("ZIPPA_TEST_POSTGRES_URL", "app").await;

    let variables = connection
        .server_variables()
        .await
        .expect("could not read the server variables");
    let row = variables
        .rows
        .iter()
        .find(|row| row[0].as_deref() == Some("max_connections"))
        .expect("max_connections should be in pg_settings");
    assert!(row[1].is_some(), "max_connections should have a value");

    connection.close().await;
}

/// `connection_info` reads the version, the database, its size, the
/// uptime, and the connection count in one query — the database is the one
/// the live helper connects to, and a fresh server has at least the test
/// connection itself connected.
#[tokio::test]
#[ignore = "needs a live Postgres server; see live_postgres_processes_lists_and_kills_another_connection"]
async fn live_postgres_connection_info_reports_all_facts() {
    let connection = live_postgres("ZIPPA_TEST_POSTGRES_URL", "app").await;

    let info = connection
        .connection_info()
        .await
        .expect("could not read the connection information");
    assert_eq!(info.engine, Engine::Postgres);
    assert!(
        !info.version.is_empty(),
        "version() should report something"
    );
    assert_eq!(info.database.as_deref(), Some("app"));
    assert!(
        info.size_bytes.is_some_and(|size| size > 0),
        "pg_database_size should report the app database's size"
    );
    assert!(
        info.uptime_secs.is_some(),
        "the server has been up for some number of seconds"
    );
    assert!(
        info.connections.is_some_and(|count| count >= 1),
        "at least this connection is connected"
    );

    connection.close().await;
}

/// `users` lists the roles, and a created role can have its password set and
/// be dropped again.
#[tokio::test]
#[ignore = "needs a live Postgres server; see live_postgres_processes_lists_and_kills_another_connection"]
async fn live_postgres_users_lists_creates_and_drops_a_role() {
    let connection = live_postgres("ZIPPA_TEST_POSTGRES_URL", "app").await;
    let name = format!("zippa_test_role_{}", Uuid::new_v4().simple());

    let names = user_names(&connection).await;
    assert!(
        names.iter().any(|n| n == "postgres"),
        "the test superuser should be listed"
    );

    connection
        .create_user(&name, None, "s3cret")
        .await
        .expect("creating the role failed");
    let names = user_names(&connection).await;
    assert!(
        names.iter().any(|n| n == &name),
        "the new role should be listed"
    );

    connection
        .set_user_password(&name, None, "n3w")
        .await
        .expect("setting the password failed");
    assert_passwords_not_logged(&connection, &["s3cret", "n3w"]);

    connection
        .drop_user(&name, None)
        .await
        .expect("dropping the role failed");
    let names = user_names(&connection).await;
    assert!(
        names.iter().all(|n| n != &name),
        "the dropped role should be gone"
    );

    connection.close().await;
}

/// `all_foreign_keys` reads the whole relation graph in one query: two tables
/// joined by a foreign key come back as one relation.
#[tokio::test]
#[ignore = "needs a live Postgres server; see live_postgres_processes_lists_and_kills_another_connection"]
async fn live_postgres_all_foreign_keys_lists_a_created_relation() {
    let connection = live_postgres("ZIPPA_TEST_POSTGRES_URL", "app").await;
    let parent = format!("zippa_erd_parent_{}", Uuid::new_v4().simple());
    let child = format!("zippa_erd_child_{}", Uuid::new_v4().simple());

    connection
        .run_query(&format!("CREATE TABLE {parent} (id INTEGER PRIMARY KEY)"))
        .await
        .expect("creating the parent table failed");
    connection
        .run_query(&format!(
            "CREATE TABLE {child} (id INTEGER PRIMARY KEY, parent_id INTEGER REFERENCES {parent}(id))"
        ))
        .await
        .expect("creating the child table failed");

    let relations = connection
        .all_foreign_keys()
        .await
        .expect("reading the foreign keys failed");
    let relation = relations
        .iter()
        .find(|relation| relation.from.name == child)
        .expect("the new foreign key should be listed");
    assert_eq!(relation.to.name, parent);
    // `objects` lists a `public` table without its schema, and the diagram
    // matches relations to tables by schema and name.
    assert_eq!(
        (
            relation.from.schema.as_deref(),
            relation.to.schema.as_deref()
        ),
        (None, None),
        "tables in public are schema-less, as `objects` lists them"
    );

    let tables = connection
        .erd_tables()
        .await
        .expect("reading the tables' columns failed");
    let parent_table = tables
        .iter()
        .find(|table| table.name == parent)
        .expect("the parent table should be listed");
    assert_eq!(parent_table.columns.len(), 1);
    assert!(parent_table.columns[0].primary_key, "id is the primary key");
    let child_table = tables
        .iter()
        .find(|table| table.name == child)
        .expect("the child table should be listed");
    assert_eq!(
        child_table
            .columns
            .iter()
            .map(|column| (column.name.as_str(), column.primary_key))
            .collect::<Vec<_>>(),
        [("id", true), ("parent_id", false)]
    );
    assert_eq!(relation.from_columns, ["parent_id"]);
    assert_eq!(relation.to_columns, ["id"]);

    connection
        .run_query(&format!("DROP TABLE {child}, {parent}"))
        .await
        .expect("cleaning up failed");
    connection.close().await;
}

/// `query_digest` is opt-in instrumentation on both engines, so a fresh test
/// server may or may not have it installed/on; this checks the call never
/// errors and, whichever branch it takes, that the branch itself is shaped
/// right — `pg_stat_statements`' own columns when available, a reason naming
/// the extension when not.
#[tokio::test]
#[ignore = "needs a live Postgres server; see live_postgres_processes_lists_and_kills_another_connection"]
async fn live_postgres_query_digest_reports_availability() {
    let connection = live_postgres("ZIPPA_TEST_POSTGRES_URL", "app").await;

    // Give the digest something to have counted, if the extension happens
    // to be installed and tracking this session.
    let _ = connection.run_query_with("SELECT 1", Vec::new()).await;

    match connection
        .query_digest()
        .await
        .expect("query_digest should not error, extension or not")
    {
        QueryDigest::Available(result) => {
            assert!(
                result.columns.contains(&"query".to_string()),
                "the digest should carry pg_stat_statements' own columns"
            );
        }
        QueryDigest::Unavailable(reason) => {
            assert!(reason.contains("pg_stat_statements"));
        }
    }

    connection.close().await;
}

/// A script inside a transaction carries on past a skipped failure on
/// Postgres, which aborts the whole transaction at an error unless the
/// statement ran under a savepoint. Runs against `compose.yaml`'s Postgres:
///
/// ```text
/// ./script/test-db up
/// cargo test -- --ignored live_postgres_a_script
/// ```
#[tokio::test]
#[ignore = "needs a live Postgres server; see the doc comment"]
async fn live_postgres_a_script_carries_on_past_a_skipped_failure() {
    let connection = Arc::new(live_postgres("ZIPPA_TEST_POSTGRES_URL", "app").await);

    // A temporary table lives on the script's own connection and goes with it.
    let (run, failure) = paused(
        ScriptRun::start(
            PinnedConnection::new(connection.clone()),
            "CREATE TEMPORARY TABLE zippa_script_skip (id int PRIMARY KEY);\n\
             INSERT INTO zippa_script_skip VALUES (1);\n\
             INSERT INTO zippa_script_skip VALUES (1);\n\
             INSERT INTO zippa_script_skip VALUES (2);\n\
             SELECT COUNT(*) FROM zippa_script_skip;",
            &[],
            ScriptMode::Transaction,
            OnFailure::Ask,
        )
        .await,
    );
    assert_eq!(failure.index, 2, "the duplicate key should fail");

    let outcome = finished(run.resume(Decision::Skip).await);
    assert_eq!(
        outcome.results.last().map(|result| result.rows.clone()),
        Some(vec![vec![Some("2".to_string())]]),
        "the transaction should have carried on past the failure"
    );

    connection.close().await;
}

/// Postgres reports a transaction an error has aborted, and a pinned tab
/// reads that back as failed until it is rolled back. Runs against
/// `compose.yaml`'s Postgres:
///
/// ```text
/// ./script/test-db up
/// cargo test -- --ignored live_postgres_a_pinned
/// ```
#[tokio::test]
#[ignore = "needs a live Postgres server; see the doc comment"]
async fn live_postgres_a_pinned_tab_reports_a_failed_transaction() {
    let connection = Arc::new(live_postgres("ZIPPA_TEST_POSTGRES_URL", "app").await);
    let pinned = PinnedConnection::new(connection.clone());

    pinned
        .run_query_with("SET search_path = pg_catalog", &[])
        .await
        .expect("set");
    let path = pinned
        .run_query_with("SHOW search_path", &[])
        .await
        .expect("show");
    assert_eq!(
        path.rows,
        [[Some("pg_catalog".to_string())]],
        "SET lasts between runs"
    );

    pinned.run_query_with("BEGIN", &[]).await.expect("begin");
    assert_eq!(pinned.state(), TxnState::Open);
    pinned
        .run_query_with("SELECT * FROM zippa_not_a_table", &[])
        .await
        .expect_err("no such table");
    assert_eq!(pinned.state(), TxnState::Failed);
    pinned
        .run_query_with("ROLLBACK", &[])
        .await
        .expect("rollback");
    assert_eq!(pinned.state(), TxnState::Idle);

    drop(pinned);
    connection.close().await;
}

/// MySQL's agent session is a read-only transaction too.
/// `cargo test -- --ignored live_mysql_an_agent`.
#[tokio::test]
#[ignore = "needs a live MySQL server; see live_mysql_routines_carry_their_argument_types"]
async fn live_mysql_an_agent_read_runs_in_a_read_only_transaction() {
    let (connection, pool) = live_mysql().await;
    for sql in [
        "DROP TABLE IF EXISTS zippa_agent_probe",
        "CREATE TABLE zippa_agent_probe (id INT PRIMARY KEY)",
    ] {
        pool.execute(AssertSqlSafe(sql.to_string()))
            .await
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }

    let refused = connection
        .run_query_as_agent("INSERT INTO zippa_agent_probe VALUES (1)", 100)
        .await;
    assert!(
        refused.is_err(),
        "the agent's session should refuse a write"
    );
    let count = connection
        .run_query_as_agent("SELECT count(*) FROM zippa_agent_probe", 100)
        .await
        .expect("a read should run");
    assert_eq!(count.rows[0][0].as_deref(), Some("0"));

    pool.execute("DROP TABLE zippa_agent_probe")
        .await
        .expect("could not drop the probe table");
    connection.close().await;
}

/// The same cap on MySQL, whose text of a cut result is left unread on a
/// connection that is then closed.
/// `cargo test -- --ignored live_mysql_an_agent`.
#[tokio::test]
#[ignore = "needs a live MySQL server; see live_mysql_routines_carry_their_argument_types"]
async fn live_mysql_an_agent_read_stops_at_its_cap() {
    let (connection, _pool) = live_mysql().await;
    let read = connection
        .run_query_as_agent(
            // A cross join of the recursion's 1000-row limit: a million
            // rows.
            "WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM n WHERE x < 1000) \
             SELECT a.x FROM n AS a CROSS JOIN n AS b",
            101,
        )
        .await
        .expect("a capped read should run");
    assert_eq!(read.rows.len(), 101);
    let after = connection
        .run_query("SELECT 1")
        .await
        .expect("the pool should still answer");
    assert_eq!(after.rows[0][0].as_deref(), Some("1"));
    connection.close().await;
}

/// The same round trip on a live MySQL server, where the decode is `UNHEX`.
/// The staged hex and the key are bound the way `update_statement` binds
/// them; the fixture goes through the pool's text protocol.
///
/// Ignored by default because it needs a server; see
/// `live_mysql_routines_carry_their_argument_types` for how to start one.
/// `cargo test -- --ignored live_mysql_binary`.
#[tokio::test]
#[ignore = "needs a live MySQL server; see live_mysql_routines_carry_their_argument_types"]
async fn live_mysql_binary_value_round_trips_through_unhex() {
    let (connection, pool) = live_mysql().await;

    for sql in [
        "DROP TABLE IF EXISTS zippa_binary_probe",
        "CREATE TABLE zippa_binary_probe (id INT PRIMARY KEY, payload BLOB)",
        "INSERT INTO zippa_binary_probe VALUES (1, x'001122')",
    ] {
        pool.execute(AssertSqlSafe(sql.to_string()))
            .await
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }

    let sql = format!(
        "UPDATE zippa_binary_probe SET payload = {} WHERE id = ?",
        binary_placeholder(Engine::MySql, 1)
    );
    let rows = connection
        .execute(
            &sql,
            vec![Some("deadbeef".to_string()), Some("1".to_string())],
        )
        .await
        .expect("the update failed");
    assert_eq!(rows, 1);

    let bytes = connection
        .fetch_binary(
            "SELECT payload FROM zippa_binary_probe WHERE id = ?",
            vec![Some("1".to_string())],
        )
        .await
        .expect("fetch failed");
    assert_eq!(bytes, Some(vec![0xde, 0xad, 0xbe, 0xef]));

    pool.execute(AssertSqlSafe("DROP TABLE zippa_binary_probe".to_string()))
        .await
        .expect("could not drop the probe table");
    connection.close().await;
    pool.close().await;
}

/// A live MySQL server, for the metadata paths SQLite cannot exercise.
///
/// Ignored by default because it needs a server. Start one and run it:
///
/// ```text
/// docker run --rm -d -p 3307:3306 --name zippa-mysql \
///   -e MYSQL_ROOT_PASSWORD=secret -e MYSQL_DATABASE=app mysql:8.4
/// cargo test -- --ignored live_mysql
/// ```
///
/// `ZIPPA_TEST_MYSQL_URL` (default `mysql://root:secret@127.0.0.1:3307/app`)
/// points the test at another server.
#[tokio::test]
#[ignore = "needs a live MySQL server; see the doc comment"]
async fn live_mysql_routines_carry_their_argument_types() {
    let (connection, pool) = live_mysql().await;

    // `CREATE FUNCTION` is refused by the prepared-statement protocol, so the
    // fixture goes through the pool's text protocol instead.
    //
    // One of each shape `information_schema.parameters` spells: a function with
    // an argument, a function with none, and a procedure with two.
    for sql in [
        "DROP FUNCTION IF EXISTS zippa_add_one",
        "DROP FUNCTION IF EXISTS zippa_no_args",
        "DROP PROCEDURE IF EXISTS zippa_do_thing",
        "CREATE FUNCTION zippa_add_one(x INT) RETURNS INT DETERMINISTIC RETURN x + 1",
        "CREATE FUNCTION zippa_no_args() RETURNS INT DETERMINISTIC RETURN 42",
        "CREATE PROCEDURE zippa_do_thing(IN p_id INT, OUT p_name VARCHAR(50)) \
         SELECT p_id INTO p_name",
    ] {
        pool.execute(AssertSqlSafe(sql.to_string()))
            .await
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }

    let routines = connection
        .stored_objects()
        .await
        .expect("could not read the routines");
    let label = |name: &str| {
        routines
            .iter()
            .find(|routine| routine.name == name)
            .map(|routine| routine.label())
    };

    assert_eq!(
        label("zippa_add_one").as_deref(),
        Some("zippa_add_one(int)")
    );
    assert_eq!(label("zippa_no_args").as_deref(), Some("zippa_no_args()"));
    assert_eq!(
        label("zippa_do_thing").as_deref(),
        Some("zippa_do_thing(int, varchar(50))"),
        "a procedure's parameter types should tell it from another signature"
    );

    connection.close().await;
    pool.close().await;
}

/// A column edit on MySQL restates the whole column (`MODIFY`/`CHANGE
/// COLUMN`), so anything `table_schema` does not carry into
/// `ColumnDef::mysql_extra` would silently vanish from a real edit even
/// though the generator that reads it is only unit-tested against literal
/// `ColumnDef`s. This is the read half: that a live server's
/// `information_schema.columns` actually comes back shaped the way
/// `mysql::columns_sql`/`schema::parse_columns` assume.
#[tokio::test]
#[ignore = "needs a live MySQL server; see live_mysql_routines_carry_their_argument_types"]
async fn live_mysql_table_schema_carries_auto_increment_collation_comment_and_on_update() {
    let (connection, pool) = live_mysql().await;

    for sql in [
        "DROP TABLE IF EXISTS zippa_probe",
        "CREATE TABLE zippa_probe (
            id INT AUTO_INCREMENT PRIMARY KEY,
            name VARCHAR(50) COLLATE utf8mb4_bin COMMENT 'the display name',
            updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP \
                ON UPDATE CURRENT_TIMESTAMP,
            price DECIMAL(10,2),
            tax DECIMAL(10,2) GENERATED ALWAYS AS (price * 0.1) STORED,
            plain INT DEFAULT 5
        )",
    ] {
        pool.execute(AssertSqlSafe(sql.to_string()))
            .await
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }

    let object = DatabaseObject {
        schema: None,
        name: "zippa_probe".to_string(),
        kind: ObjectKind::Table,
    };
    let schema = connection
        .table_schema(&object)
        .await
        .expect("could not read the table schema");
    let column = |name: &str| {
        schema
            .columns
            .iter()
            .find(|column| column.name == name)
            .unwrap_or_else(|| panic!("no {name} column came back"))
    };

    assert!(column("id").mysql_extra.auto_increment);
    assert!(!column("plain").mysql_extra.auto_increment);

    assert!(column("updated_at").mysql_extra.on_update_current_timestamp);
    assert!(!column("plain").mysql_extra.on_update_current_timestamp);

    assert_eq!(
        column("name").mysql_extra.collation.as_deref(),
        Some("utf8mb4_bin")
    );
    assert_eq!(column("plain").mysql_extra.collation, None);

    assert_eq!(
        column("name").mysql_extra.comment.as_deref(),
        Some("the display name")
    );
    assert_eq!(
        column("plain").mysql_extra.comment,
        None,
        "an unset comment is an empty string on the server, folded to None"
    );

    assert_eq!(
        column("tax").mysql_extra.generation_expression.as_deref(),
        Some("(`price` * 0.1)")
    );
    assert_eq!(column("plain").mysql_extra.generation_expression, None);

    connection.close().await;
    pool.execute(AssertSqlSafe("DROP TABLE zippa_probe".to_string()))
        .await
        .expect("could not drop the probe table");
    pool.close().await;
}

/// MySQL 8 generates a self-signed certificate on first start, so the compose
/// server takes `Require` (and the session really is encrypted) but fails
/// `VerifyCa`, whose whole point is to refuse an authority nobody trusts.
#[tokio::test]
#[ignore = "needs a live MySQL server; see the doc comment"]
async fn live_mysql_ssl_require_encrypts_and_verify_refuses_a_self_signed_server() {
    let (mut config, password) = live_mysql_config();
    config.ssl = SslConfig {
        mode: SslMode::Require,
        ..SslConfig::default()
    };
    let encrypted = Connection::open(config.clone(), Some(password.clone()))
        .await
        .expect("Require should connect to a server with SSL");
    let cipher = encrypted
        .run_query("SHOW SESSION STATUS LIKE 'Ssl_cipher'")
        .await
        .expect("could not read the session's cipher");
    assert!(
        cipher.rows[0][1]
            .as_deref()
            .is_some_and(|cipher| !cipher.is_empty()),
        "a Require session should be encrypted: {:?}",
        cipher.rows
    );
    encrypted.close().await;

    config.ssl.mode = SslMode::VerifyCa;
    assert!(
        Connection::open(config, Some(password)).await.is_err(),
        "VerifyCa should refuse a self-signed certificate"
    );
}

/// The same, for MySQL's `information_schema.processlist` and `KILL`.
#[tokio::test]
#[ignore = "needs a live MySQL server; see live_mysql_routines_carry_their_argument_types"]
async fn live_mysql_processes_lists_and_kills_another_connection() {
    let (watcher, _watcher_pool) = live_mysql().await;
    let (victim, _victim_pool) = live_mysql().await;

    let sleeper = tokio::spawn(async move { victim.run_query("SELECT sleep(30)").await });
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    let processes = watcher
        .processes()
        .await
        .expect("could not read the process list");
    let row = processes
        .rows
        .iter()
        .find(|row| {
            row.get(7)
                .and_then(|cell| cell.as_deref())
                .is_some_and(|info| info.contains("sleep"))
        })
        .expect("the sleeping connection should be in the process list");
    let id = row[0].clone().expect("an id");

    watcher
        .kill_process(&id)
        .await
        .expect("could not end the sleeping connection");

    let result = sleeper.await.expect("the task itself should not panic");
    assert!(
        result.is_err(),
        "the killed connection's query should have been interrupted"
    );

    watcher.close().await;
}

/// The same, for MySQL's `max_execution_time` (milliseconds, reads only).
#[tokio::test]
#[ignore = "needs a live MySQL server; see live_mysql_routines_carry_their_argument_types"]
async fn live_mysql_statement_timeout_is_set_on_the_session() {
    let (connection, _pool) = live_mysql_with(|config| config.statement_timeout = Some(2)).await;

    let shown = connection
        .run_query("SELECT @@SESSION.max_execution_time")
        .await
        .expect("could not read the setting");
    assert_eq!(shown.rows[0][0].as_deref(), Some("2000"));

    connection.close().await;
}

/// The same, for MySQL's `VERSION()`/`DATABASE()`/information_schema size
/// plus `SHOW GLOBAL STATUS` for the uptime and connection count.
#[tokio::test]
#[ignore = "needs a live MySQL server; see live_mysql_routines_carry_their_argument_types"]
async fn live_mysql_connection_info_reports_all_facts() {
    let (connection, _pool) = live_mysql().await;

    let info = connection
        .connection_info()
        .await
        .expect("could not read the connection information");
    assert_eq!(info.engine, Engine::MySql);
    assert!(
        !info.version.is_empty(),
        "VERSION() should report something"
    );
    assert!(
        info.database.is_some(),
        "the live helper connects to a database"
    );
    assert!(
        info.size_bytes.is_some(),
        "the size sum should come back even for an empty database"
    );
    assert!(
        info.uptime_secs.is_some(),
        "SHOW GLOBAL STATUS should report Uptime"
    );
    assert!(
        info.connections.is_some_and(|count| count >= 1),
        "at least this connection is connected"
    );

    connection.close().await;
}

/// The same, for MySQL's `performance_schema.global_variables`/`variables_info`.
#[tokio::test]
#[ignore = "needs a live MySQL server; see live_mysql_routines_carry_their_argument_types"]
async fn live_mysql_server_variables_lists_max_connections() {
    let (connection, _pool) = live_mysql().await;

    let variables = connection
        .server_variables()
        .await
        .expect("could not read the server variables");
    let row = variables
        .rows
        .iter()
        .find(|row| {
            row[0]
                .as_deref()
                .is_some_and(|name| name.eq_ignore_ascii_case("max_connections"))
        })
        .expect("max_connections should be in performance_schema.global_variables");
    assert!(row[1].is_some(), "max_connections should have a value");

    connection.close().await;
}

/// The same, for MySQL's `mysql.user` and `CREATE/ALTER/DROP USER`.
#[tokio::test]
#[ignore = "needs a live MySQL server; see live_mysql_routines_carry_their_argument_types"]
async fn live_mysql_users_lists_creates_and_drops_a_user() {
    let (connection, _pool) = live_mysql().await;
    // MySQL caps usernames at 32 chars; keep the random suffix short.
    let name = format!(
        "zippa_test_user_{}",
        &Uuid::new_v4().simple().to_string()[..8]
    );

    let names = user_names(&connection).await;
    assert!(
        names.iter().any(|n| n == "root"),
        "the test superuser should be listed"
    );

    connection
        .create_user(&name, Some("%"), "s3cret")
        .await
        .expect("creating the user failed");
    let names = user_names(&connection).await;
    assert!(
        names.iter().any(|n| n == &name),
        "the new user should be listed"
    );

    connection
        .set_user_password(&name, Some("%"), "n3w")
        .await
        .expect("setting the password failed");
    assert_passwords_not_logged(&connection, &["s3cret", "n3w"]);

    connection
        .drop_user(&name, Some("%"))
        .await
        .expect("dropping the user failed");
    let names = user_names(&connection).await;
    assert!(
        names.iter().all(|n| n != &name),
        "the dropped user should be gone"
    );

    connection.close().await;
}

/// The same, for MySQL's `information_schema`.
#[tokio::test]
#[ignore = "needs a live MySQL server; see live_mysql_routines_carry_their_argument_types"]
async fn live_mysql_all_foreign_keys_lists_a_created_relation() {
    let (connection, _pool) = live_mysql().await;
    let parent = format!("zippa_erd_parent_{}", Uuid::new_v4().simple());
    let child = format!("zippa_erd_child_{}", Uuid::new_v4().simple());

    connection
        .run_query(&format!("CREATE TABLE {parent} (id INTEGER PRIMARY KEY)"))
        .await
        .expect("creating the parent table failed");
    connection
        .run_query(&format!(
            "CREATE TABLE {child} (id INTEGER PRIMARY KEY, parent_id INTEGER, FOREIGN KEY (parent_id) REFERENCES {parent}(id))"
        ))
        .await
        .expect("creating the child table failed");

    let relations = connection
        .all_foreign_keys()
        .await
        .expect("reading the foreign keys failed");
    let relation = relations
        .iter()
        .find(|relation| relation.from.name == child)
        .expect("the new foreign key should be listed");
    assert_eq!(relation.to.name, parent);
    assert_eq!(
        relation.to.schema, None,
        "same-database references are schema-less"
    );

    let tables = connection
        .erd_tables()
        .await
        .expect("reading the tables' columns failed");
    let parent_table = tables
        .iter()
        .find(|table| table.name == parent)
        .expect("the parent table should be listed");
    assert_eq!(parent_table.columns.len(), 1);
    assert!(parent_table.columns[0].primary_key, "id is the primary key");
    let child_table = tables
        .iter()
        .find(|table| table.name == child)
        .expect("the child table should be listed");
    assert_eq!(
        child_table
            .columns
            .iter()
            .map(|column| (column.name.as_str(), column.primary_key))
            .collect::<Vec<_>>(),
        [("id", true), ("parent_id", false)]
    );
    assert_eq!(relation.from_columns, ["parent_id"]);
    assert_eq!(relation.to_columns, ["id"]);

    connection
        .run_query(&format!("DROP TABLE {child}, {parent}"))
        .await
        .expect("cleaning up failed");
    connection.close().await;
}

/// The same, for MySQL's `performance_schema.events_statements_summary_by_digest`.
#[tokio::test]
#[ignore = "needs a live MySQL server; see live_mysql_routines_carry_their_argument_types"]
async fn live_mysql_query_digest_reports_availability() {
    let (connection, _pool) = live_mysql().await;

    let _ = connection.run_query_with("SELECT 1", Vec::new()).await;

    match connection
        .query_digest()
        .await
        .expect("query_digest should not error, performance_schema on or off")
    {
        QueryDigest::Available(result) => {
            assert!(
                result.columns.contains(&"digest_text".to_string()),
                "the digest should carry events_statements_summary_by_digest's own columns"
            );
        }
        QueryDigest::Unavailable(reason) => {
            assert!(reason.contains("performance_schema"));
        }
    }

    connection.close().await;
}

/// A MySQL script of plain row changes rolls back as a whole. Runs against
/// `compose.yaml`'s MySQL:
///
/// ```text
/// ./script/test-db up
/// cargo test -- --ignored live_mysql_a_script
/// ```
#[tokio::test]
#[ignore = "needs a live MySQL server; see the doc comment"]
async fn live_mysql_a_script_of_row_changes_rolls_back() {
    let (connection, pool) = live_mysql().await;
    let connection = Arc::new(connection);

    pool.execute("DROP TABLE IF EXISTS zippa_script_rollback")
        .await
        .expect("could not clear the fixture");
    pool.execute("CREATE TABLE zippa_script_rollback (id int PRIMARY KEY) ENGINE=InnoDB")
        .await
        .expect("could not create the fixture");

    let (run, _) = paused(
        ScriptRun::start(
            PinnedConnection::new(connection.clone()),
            "INSERT INTO zippa_script_rollback VALUES (1);\n\
             INSERT INTO not_a_table VALUES (1);",
            &[],
            ScriptMode::Transaction,
            OnFailure::Ask,
        )
        .await,
    );
    let outcome = finished(run.resume(Decision::Abort).await);
    assert!(outcome.rolled_back);

    let count = connection
        .run_query("SELECT COUNT(*) FROM zippa_script_rollback")
        .await
        .expect("the count failed");
    assert_eq!(count.rows, [[Some("0".to_string())]]);

    pool.execute("DROP TABLE zippa_script_rollback").await.ok();
    connection.close().await;
}

/// A script run without a transaction can hold the statements MySQL's
/// prepared protocol refuses, which is what that mode is for. Runs against
/// `compose.yaml`'s MySQL:
///
/// ```text
/// ./script/test-db up
/// cargo test -- --ignored live_mysql_an_autocommit
/// ```
#[tokio::test]
#[ignore = "needs a live MySQL server; see the doc comment"]
async fn live_mysql_an_autocommit_script_can_lock_tables() {
    let (connection, pool) = live_mysql().await;
    let connection = Arc::new(connection);

    pool.execute("DROP TABLE IF EXISTS zippa_script_locks")
        .await
        .expect("could not clear the fixture");
    pool.execute("CREATE TABLE zippa_script_locks (id int PRIMARY KEY) ENGINE=InnoDB")
        .await
        .expect("could not create the fixture");

    let outcome = finished(
        ScriptRun::start(
            PinnedConnection::new(connection.clone()),
            "LOCK TABLES zippa_script_locks WRITE;\n\
             INSERT INTO zippa_script_locks VALUES (1);\n\
             UNLOCK TABLES;\n\
             SELECT COUNT(*) FROM zippa_script_locks;",
            &[],
            ScriptMode::Autocommit,
            OnFailure::Ask,
        )
        .await,
    );
    assert!(outcome.failures.is_empty(), "{:?}", outcome.failures);
    assert_eq!(
        outcome.results.last().map(|result| result.rows.clone()),
        Some(vec![vec![Some("1".to_string())]]),
    );

    pool.execute("DROP TABLE zippa_script_locks").await.ok();
    connection.close().await;
}

/// A query tab on MySQL reports the warnings its statement produced, read
/// back with `SHOW WARNINGS` on the tab's pinned connection: division by
/// zero returns `NULL` with warning 1365 rather than an error, and a clean
/// statement leaves no messages behind. Runs against `compose.yaml`'s
/// MySQL:
///
/// ```text
/// ./script/test-db up
/// cargo test -- --ignored live_mysql_a_pinned_tab_reports_warnings
/// ```
#[tokio::test]
#[ignore = "needs a live MySQL server; see the doc comment"]
async fn live_mysql_a_pinned_tab_reports_warnings() {
    let (connection, _pool) = live_mysql().await;
    let connection = Arc::new(connection);
    let pinned = PinnedConnection::new(connection.clone());

    let warned = pinned
        .run_query_with("SELECT 1 / 0 AS quotient", &[])
        .await
        .expect("division by zero is a warning, not an error");
    assert_eq!(warned.rows, [[None]], "division by zero reads back as NULL");
    assert_eq!(
        warned.messages.len(),
        1,
        "one warning for the division, got {:?}",
        warned.messages
    );
    assert_eq!(warned.messages[0].level, "Warning");
    assert_eq!(warned.messages[0].code.as_deref(), Some("1365"));
    assert!(
        warned.messages[0].text.contains("Division by 0"),
        "unexpected warning text: {}",
        warned.messages[0].text
    );

    // The next statement's clean run does not inherit the last one's
    // warnings: `SHOW WARNINGS` answers for the statement just run.
    let clean = pinned
        .run_query_with("SELECT 1 AS one", &[])
        .await
        .expect("a clean statement");
    assert!(
        clean.messages.is_empty(),
        "a clean statement leaves no messages, got {:?}",
        clean.messages
    );

    drop(pinned);
    connection.close().await;
}

/// A query tab on MySQL can run `BEGIN`, `USE`, and `LOCK TABLES` — statements
/// the prepared protocol refuses — and they last from one run to the next on
/// its pinned connection, which follows the transaction from the statements
/// themselves. Runs against `compose.yaml`'s MySQL:
///
/// ```text
/// ./script/test-db up
/// cargo test -- --ignored live_mysql_a_pinned
/// ```
#[tokio::test]
#[ignore = "needs a live MySQL server; see the doc comment"]
async fn live_mysql_a_pinned_tab_holds_a_transaction_and_its_locks() {
    let (connection, pool) = live_mysql().await;
    let connection = Arc::new(connection);

    pool.execute("DROP TABLE IF EXISTS zippa_pinned")
        .await
        .expect("could not clear the fixture");
    pool.execute("CREATE TABLE zippa_pinned (id int PRIMARY KEY) ENGINE=InnoDB")
        .await
        .expect("could not create the fixture");

    let pinned = PinnedConnection::new(connection.clone());
    pinned.run_query_with("BEGIN", &[]).await.expect("begin");
    assert_eq!(pinned.state(), TxnState::Open);
    pinned
        .run_query_with("INSERT INTO zippa_pinned VALUES (1)", &[])
        .await
        .expect("insert");
    let elsewhere = connection
        .run_query("SELECT COUNT(*) FROM zippa_pinned")
        .await
        .expect("count from the pool");
    assert_eq!(
        elsewhere.rows,
        [[Some("0".to_string())]],
        "not committed yet"
    );
    pinned
        .run_query_with("ROLLBACK", &[])
        .await
        .expect("rollback");
    assert_eq!(pinned.state(), TxnState::Idle);

    pinned.run_query_with("USE app", &[]).await.expect("use");
    pinned
        .run_query_with("LOCK TABLES zippa_pinned WRITE", &[])
        .await
        .expect("lock tables");
    pinned
        .run_query_with("INSERT INTO zippa_pinned VALUES (2)", &[])
        .await
        .expect("insert under the lock, on the same connection");
    pinned
        .run_query_with("UNLOCK TABLES", &[])
        .await
        .expect("unlock");

    let count = connection
        .run_query("SELECT COUNT(*) FROM zippa_pinned")
        .await
        .expect("the count failed");
    assert_eq!(count.rows, [[Some("1".to_string())]]);

    drop(pinned);
    pool.execute("DROP TABLE zippa_pinned").await.ok();
    connection.close().await;
}

/// A `mysqldump` file imports, `LOCK TABLES` and all — statements MySQL's
/// prepared protocol refuses. Runs against `compose.yaml`'s MySQL:
///
/// ```text
/// ./script/test-db up
/// cargo test -- --ignored live_mysql_a_mysqldump
/// ```
#[tokio::test]
#[ignore = "needs a live MySQL server; see the doc comment"]
async fn live_mysql_a_mysqldump_file_imports_with_its_table_locks() {
    use crate::db::{ImportRequest, OnError};

    let (connection, pool) = live_mysql().await;
    pool.execute("DROP TABLE IF EXISTS zippa_import_locks")
        .await
        .expect("could not clear the fixture");

    let path = env::temp_dir().join(format!("zippa-dump-{}.sql", Uuid::new_v4()));
    std::fs::write(
        &path,
        "CREATE TABLE `zippa_import_locks` (`id` int NOT NULL, PRIMARY KEY (`id`)) ENGINE=InnoDB;\n\
         LOCK TABLES `zippa_import_locks` WRITE;\n\
         /*!40000 ALTER TABLE `zippa_import_locks` DISABLE KEYS */;\n\
         INSERT INTO `zippa_import_locks` VALUES (1),(2);\n\
         /*!40000 ALTER TABLE `zippa_import_locks` ENABLE KEYS */;\n\
         UNLOCK TABLES;\n",
    )
    .expect("could not write the dump");

    let (sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
    let summary = connection
        .import_dump(
            ImportRequest {
                path: path.clone(),
                on_error: OnError::Stop,
            },
            sender,
        )
        .await;
    std::fs::remove_file(&path).ok();
    let summary = summary.expect("the dump should import");
    assert!(summary.errors.is_empty(), "{:?}", summary.errors);

    let count = connection
        .run_query("SELECT COUNT(*) FROM zippa_import_locks")
        .await
        .expect("the count failed");
    assert_eq!(count.rows, [[Some("2".to_string())]]);

    pool.execute("DROP TABLE zippa_import_locks").await.ok();
    connection.close().await;
}

/// A dump in `pg_dump`'s plain format, where every `COPY` sits behind a
/// `-- Data for Name: …` comment block, has its data streamed through `COPY`
/// rather than read as SQL. Against `compose.yaml`'s `postgres` service.
#[tokio::test]
#[ignore = "needs a live Postgres server; see the doc comment"]
async fn live_postgres_a_pg_dump_file_imports_its_copy_data() {
    use crate::db::{ImportRequest, OnError};

    let connection = live_postgres("ZIPPA_TEST_POSTGRES_URL", "app").await;
    connection
        .execute("DROP TABLE IF EXISTS zippa_import_copy", vec![])
        .await
        .expect("could not clear the fixture");

    let path = env::temp_dir().join(format!("zippa-dump-{}.sql", Uuid::new_v4()));
    std::fs::write(
        &path,
        "--\n-- PostgreSQL database dump\n--\n\n\
         SET client_encoding = 'UTF8';\n\n\
         --\n-- Name: zippa_import_copy; Type: TABLE; Schema: public; Owner: postgres\n--\n\n\
         CREATE TABLE public.zippa_import_copy (\n    id integer NOT NULL,\n    name text\n);\n\n\
         --\n-- Data for Name: zippa_import_copy; Type: TABLE DATA; Schema: public; Owner: postgres\n--\n\n\
         COPY public.zippa_import_copy (id, name) FROM stdin;\n\
         1\tone; two\n\
         2\t\\N\n\
         3\tthree\n\
         \\.\n\n\n\
         --\n-- PostgreSQL database dump complete\n--\n\n",
    )
    .expect("could not write the dump");

    let (sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
    let summary = connection
        .import_dump(
            ImportRequest {
                path: path.clone(),
                on_error: OnError::Rollback,
            },
            sender,
        )
        .await;
    std::fs::remove_file(&path).ok();
    let summary = summary.expect("the dump should import");
    assert!(summary.errors.is_empty(), "{:?}", summary.errors);

    let rows = connection
        .run_query("SELECT id, name FROM zippa_import_copy ORDER BY id")
        .await
        .expect("the read failed");
    assert_eq!(
        rows.rows,
        [
            [Some("1".to_string()), Some("one; two".to_string())],
            [Some("2".to_string()), None],
            [Some("3".to_string()), Some("three".to_string())],
        ]
    );

    connection
        .execute("DROP TABLE zippa_import_copy", vec![])
        .await
        .ok();
    connection.close().await;
}

/// A wrong SSH password is refused with a message that says so, before the
/// database is ever asked.
#[tokio::test]
#[ignore = "needs a live SSH jump host; see `live_ssh`"]
async fn live_ssh_tunnel_reports_a_refused_password() {
    let (ssh, _, target_host, target_port) = live_ssh();
    let (mut config, password) = live_postgres_config("ZIPPA_TEST_POSTGRES_URL", "app");
    config.host = target_host;
    config.port = target_port;
    config.ssh = ssh;

    let error = Connection::open_with(
        config,
        Credentials {
            password: Some(password),
            ssh: Some("not-the-password".into()),
        },
    )
    .await
    .expect_err("a wrong SSH password should be refused");
    assert!(
        format!("{error:#}").contains("refused the password"),
        "{error:#}"
    );
}

/// A streamed read hands over every row of a result far larger than any page,
/// and one stopped partway closes its connection rather than leaving the
/// pool to drain the rest. Against `compose.yaml`'s `postgres` service.
#[tokio::test]
#[ignore = "needs a live Postgres server; see the doc comment"]
async fn live_postgres_a_streamed_read_hands_over_every_row() {
    let connection = live_postgres("ZIPPA_TEST_POSTGRES_URL", "app").await;
    let sql = "SELECT g FROM generate_series(1, 1000000) AS g";

    let mut sink = CountingSink::new(None);
    let rows = connection
        .stream_query_with(sql, Vec::new(), &mut sink)
        .await
        .expect("the streamed read should run");
    assert_eq!(rows, 1_000_000);
    assert_eq!(sink.columns, ["g"]);
    assert_eq!(sink.last.as_deref(), Some("1000000"));

    let mut stopped = CountingSink::new(Some(10));
    let error = connection
        .stream_query_with(sql, Vec::new(), &mut stopped)
        .await
        .expect_err("the sink stops the read");
    assert!(format!("{error:#}").contains("stopped"), "{error:#}");
    assert_eq!(stopped.rows, 10);

    let after = connection
        .run_query("SELECT 1")
        .await
        .expect("the pool should still answer");
    assert_eq!(after.rows[0][0].as_deref(), Some("1"));
    connection.close().await;
}

/// The same on MySQL. Against `compose.yaml`'s `mysql` service.
/// `cargo test -- --ignored live_mysql_a_streamed`.
#[tokio::test]
#[ignore = "needs a live MySQL server; see live_mysql_routines_carry_their_argument_types"]
async fn live_mysql_a_streamed_read_hands_over_every_row() {
    let (connection, _pool) = live_mysql().await;
    let digits = "(SELECT 0 d UNION ALL SELECT 1 UNION ALL SELECT 2 UNION ALL SELECT 3 \
                  UNION ALL SELECT 4 UNION ALL SELECT 5 UNION ALL SELECT 6 UNION ALL SELECT 7 \
                  UNION ALL SELECT 8 UNION ALL SELECT 9)";
    let sql = format!(
        "SELECT a.d + 10 * b.d + 100 * c.d + 1000 * e.d + 10000 * f.d + 1 AS n \
         FROM {digits} a, {digits} b, {digits} c, {digits} e, {digits} f ORDER BY n"
    );

    let mut sink = CountingSink::new(None);
    let rows = connection
        .stream_query_with(&sql, Vec::new(), &mut sink)
        .await
        .expect("the streamed read should run");
    assert_eq!(rows, 100_000);
    assert_eq!(sink.last.as_deref(), Some("100000"));

    let mut stopped = CountingSink::new(Some(10));
    connection
        .stream_query_with(&sql, Vec::new(), &mut stopped)
        .await
        .expect_err("the sink stops the read");

    let after = connection
        .run_query("SELECT 1")
        .await
        .expect("the pool should still answer");
    assert_eq!(after.rows[0][0].as_deref(), Some("1"));
    connection.close().await;
}
