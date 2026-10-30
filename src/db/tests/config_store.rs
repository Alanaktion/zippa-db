//! Saved connection metadata.

use super::*;

#[test]
fn a_connection_saved_before_safety_modes_reads_back_as_staged() {
    // The field is written by every save now, but files from before it
    // existed have to keep working, and they get the careful mode.
    let saved = r#"{
        "id": "00000000-0000-0000-0000-000000000001",
        "name": "old",
        "engine": "Postgres",
        "host": "localhost",
        "port": 5432,
        "username": "postgres",
        "database": "app"
    }"#;

    let config: ConnectionConfig =
        serde_json::from_str(saved).expect("an older connection should still load");
    assert_eq!(config.safety, SafetyMode::Staged);

    let written = serde_json::to_string(&ConnectionConfig {
        safety: SafetyMode::AutoApply,
        ..ConnectionConfig::new(Engine::Postgres)
    })
    .expect("the config should serialize");
    assert!(
        written.contains("\"safety\":\"AutoApply\""),
        "the mode belongs in the file: {written}"
    );
}

#[test]
fn a_connection_saved_before_colouring_reads_back_uncoloured() {
    // No colour or last_connected in a file written before either existed, so
    // both read back as absent rather than failing to parse.
    let saved = r#"{
        "id": "00000000-0000-0000-0000-000000000001",
        "name": "old",
        "engine": "Postgres",
        "host": "localhost",
        "port": 5432,
        "username": "postgres",
        "database": "app",
        "safety": "Staged"
    }"#;

    let config: ConnectionConfig =
        serde_json::from_str(saved).expect("an older connection should still load");
    assert_eq!(config.color, None);
    assert_eq!(config.last_connected, None);
}

#[test]
fn a_coloured_connection_survives_a_round_trip_through_the_file() {
    let config = ConnectionConfig {
        name: "Prod DB".into(),
        color: Some(TagColor::Red),
        last_connected: Some(
            "2024-01-02T03:04:05Z"
                .parse::<chrono::DateTime<chrono::Utc>>()
                .expect("a fixed timestamp"),
        ),
        ..ConnectionConfig::new(Engine::Postgres)
    };

    let written = serde_json::to_string(&config).expect("the config should serialize");
    let read: ConnectionConfig = serde_json::from_str(&written).expect("the config should parse");

    assert_eq!(read.color, Some(TagColor::Red));
    assert_eq!(read.last_connected, config.last_connected);
}

#[test]
fn a_connection_saved_with_a_label_still_loads() {
    // Connections once carried a free-text `tag` beside the colour; a file
    // written then must keep loading, with the colour intact.
    let saved = r#"{
        "id": "7d0f3f3e-5b1a-4c55-9f4b-0f1d2c3b4a59",
        "name": "app",
        "engine": "Postgres",
        "host": "localhost",
        "port": 5432,
        "username": "postgres",
        "database": "app",
        "tag": "Production",
        "color": "red"
    }"#;

    let config: ConnectionConfig =
        serde_json::from_str(saved).expect("a labelled connection should still load");
    assert_eq!(config.color, Some(TagColor::Red));
}

#[test]
fn tag_color_keys_serialise_as_kebab_case() {
    for color in TagColor::ALL {
        let written = serde_json::to_string(&color).expect("the colour should serialize");
        assert_eq!(written, format!("\"{}\"", color.key()));
        let read: TagColor = serde_json::from_str(&written).expect("the colour should parse");
        assert_eq!(read, color);
    }
}
