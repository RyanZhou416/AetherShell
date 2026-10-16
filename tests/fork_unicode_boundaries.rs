//! UTF-8 boundary regressions for byte-capped user text.
//!
//! Each case reaches a byte cap with CJK or emoji, where the old `&s[..N]`
//! slice split a character and panicked. The paths exercised here are pure: no
//! model, network, terminal, or credential is involved.

use aethershell::agent;
use aethershell::persistence::{Conversation, Message};
use aethershell::text::prefix_bytes;

#[test]
fn prefix_bytes_floors_cjk_and_emoji_to_a_character_boundary() {
    // 中 is three bytes: byte 100 is inside the 34th character.
    assert_eq!(prefix_bytes(&"中".repeat(34), 100).len(), 99);
    // 😀 is four bytes: byte 100 is exactly the 25th character.
    assert_eq!(prefix_bytes(&"😀".repeat(50), 100).len(), 100);
    assert_eq!(prefix_bytes("abcdef", 3), "abc");
}

#[test]
fn prefix_bytes_never_splits_a_character_at_any_budget() {
    let s = "a中😀é中";
    for budget in 0..=s.len() + 1 {
        assert!(s.is_char_boundary(prefix_bytes(s, budget).len()));
    }
}

#[test]
fn agent_plan_accepts_a_multibyte_goal() {
    // 102 bytes: past the 100-byte log cap, with byte 100 inside a character.
    let goal = "中".repeat(34);
    let plan = agent::plan(&goal).expect("a multibyte goal plans");
    assert_eq!(plan.goal, goal);
}

#[test]
fn conversation_auto_title_bounds_multibyte_content() {
    // 51 bytes: past the 50-byte guard, with byte 47 inside a character.
    let mut conv = Conversation::new("agent-1".to_string());
    conv.add_message(Message::user(&"中".repeat(17)));
    conv.auto_title();

    let title = conv
        .title
        .expect("a first user message names the conversation");
    assert!(title.len() <= 50);
    assert!(title.ends_with("..."));
}

#[test]
fn conversation_auto_title_is_byte_identical_for_ascii() {
    let mut conv = Conversation::new("agent-1".to_string());
    conv.add_message(Message::user("What is the weather like today?"));
    conv.auto_title();
    assert_eq!(
        conv.title.as_deref(),
        Some("What is the weather like today?")
    );

    let mut long = Conversation::new("agent-2".to_string());
    long.add_message(Message::user(
        "This is a very long message that should be truncated when used as a title",
    ));
    long.auto_title();

    let title = long.title.expect("title set");
    assert_eq!(title.len(), 50);
    assert_eq!(
        &title[..47],
        "This is a very long message that should be trun"
    );
    assert!(title.ends_with("..."));
}
