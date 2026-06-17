// SPDX-License-Identifier: Apache-2.0

use artesian_core::{AgentBinding, ArtesianConfig, Role};

#[test]
fn config_round_trips_through_toml() {
    let config = ArtesianConfig::memory_files(
        ".artesian",
        vec![AgentBinding {
            role: Role::Master,
            agent: "claude-code".to_string(),
            model: Some("default".to_string()),
            command: Some("claude".to_string()),
            args: vec!["--print".to_string()],
            timeout_seconds: Some(120),
        }],
    );

    let encoded = config.to_toml().expect("config should encode");
    let decoded = ArtesianConfig::from_toml(&encoded).expect("config should decode");

    assert_eq!(decoded, config);
}
