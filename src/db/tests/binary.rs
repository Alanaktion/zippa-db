//! Binary values read and written back.

use super::*;

#[tokio::test]
async fn fetch_binary_reads_the_real_bytes_a_query_only_describes() {
    let database = TempDatabase::new().await;
    let connection = Connection::open(database.config(), None)
        .await
        .expect("could not open the test database");

    // `run_query` only ever hands back `<3 bytes>`; `fetch_binary` is the
    // path that reads what those bytes actually are.
    let described = connection
        .run_query("SELECT payload FROM items WHERE id = 1")
        .await
        .expect("query failed");
    assert_eq!(described.rows[0][0], Some("<3 bytes>".to_string()));

    let bytes = connection
        .fetch_binary(
            "SELECT payload FROM items WHERE id = ?",
            vec![Some("1".to_string())],
        )
        .await
        .expect("fetch failed");
    assert_eq!(bytes, Some(vec![0x00, 0x11, 0x22]));

    // A NULL column and a row that matches nothing both come back empty
    // rather than as an error.
    let null = connection
        .fetch_binary(
            "SELECT payload FROM items WHERE id = ?",
            vec![Some("2".to_string())],
        )
        .await
        .expect("fetch failed");
    assert_eq!(null, None);

    let missing = connection
        .fetch_binary(
            "SELECT payload FROM items WHERE id = ?",
            vec![Some("99".to_string())],
        )
        .await
        .expect("fetch failed");
    assert_eq!(missing, None);

    connection.close().await;
}

#[tokio::test]
async fn a_staged_binary_value_round_trips_through_its_hex() {
    let database = TempDatabase::new().await;
    let connection = Connection::open(database.config(), None)
        .await
        .expect("could not open the test database");

    // The statement `update_statement` generates for a staged hex value on
    // the BLOB column, with the hex bound the way every other value is.
    let sql = format!(
        "UPDATE items SET payload = {} WHERE id = ?",
        binary_placeholder(Engine::Sqlite, 1)
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
            "SELECT payload FROM items WHERE id = ?",
            vec![Some("1".to_string())],
        )
        .await
        .expect("fetch failed");
    assert_eq!(bytes, Some(vec![0xde, 0xad, 0xbe, 0xef]));

    // A NULL staged on the same column decodes to NULL, not to an error.
    let rows = connection
        .execute(&sql, vec![None, Some("1".to_string())])
        .await
        .expect("clearing the value failed");
    assert_eq!(rows, 1);
    let bytes = connection
        .fetch_binary(
            "SELECT payload FROM items WHERE id = ?",
            vec![Some("1".to_string())],
        )
        .await
        .expect("fetch failed");
    assert_eq!(bytes, None);

    connection.close().await;
}
