// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn role_aliases_resolve_plain_english_and_norse_names() {
    assert_eq!("master".parse::<Role>(), Ok(Role::Master));
    assert_eq!("odin".parse::<Role>(), Ok(Role::Master));
    assert_eq!("worker".parse::<Role>(), Ok(Role::Worker));
    assert_eq!("thor".parse::<Role>(), Ok(Role::Worker));
    assert_eq!("judge".parse::<Role>(), Ok(Role::Judge));
    assert_eq!("tyr".parse::<Role>(), Ok(Role::Judge));
}

#[test]
fn thing_queue_preserves_fifo_order() {
    let first = Erindi {
        id: "0001".to_string(),
        title: "Scaffold".to_string(),
        role: Role::Worker,
        status: ErindiStatus::Todo,
    };
    let second = Erindi {
        id: "0002".to_string(),
        title: "Review".to_string(),
        role: Role::Judge,
        status: ErindiStatus::Todo,
    };
    let mut thing = Thing::default();

    thing.push(first.clone());
    thing.push(second.clone());

    assert_eq!(thing.pop_next(), Some(first));
    assert_eq!(thing.pop_next(), Some(second));
    assert_eq!(thing.pop_next(), None);
}
