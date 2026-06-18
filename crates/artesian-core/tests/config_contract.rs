// SPDX-License-Identifier: Apache-2.0

use artesian_core::{AccConfig, AgentBinding, ArtesianConfig, Role};

#[test]
fn config_round_trips_through_toml() {
    let mut config = ArtesianConfig::memory_files(
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
    config.acc.budget_tokens = 4096;
    config.acc.min_score = 0.3;

    let encoded = config.to_toml().expect("config should encode");
    let decoded = ArtesianConfig::from_toml(&encoded).expect("config should decode");

    assert_eq!(decoded, config);
    assert_eq!(decoded.acc.budget_tokens, 4096);
}

#[test]
fn acc_block_is_optional_and_defaults_apply() {
    let toml = r#"
mode = "memory"

[memory]
backend = "files"
root = ".artesian"
collection = "artesian-memory"

[[agents]]
role = "master"
agent = "claude-code"
"#;
    let config = ArtesianConfig::from_toml(toml).expect("config without [acc] should decode");
    assert_eq!(config.acc, AccConfig::default());
    assert_eq!(config.acc.budget_tokens, 2048);
    assert!(config.acc.compress_on_saturation);
}
