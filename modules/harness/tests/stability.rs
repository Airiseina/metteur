//! Prefix-cache contract.
//!
//! Providers cache the request prefix byte for byte (OpenAI/DeepSeek
//! implicitly, Anthropic at explicit breakpoints), and every hit depends on the
//! assembled system prompt being *identical* across turns. These tests pin the
//! properties a change to the prompt system must preserve.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use metteur_harness::{EnvFacts, HarnessPrompt, assembly};
use metteur_shared::config::Config;
use metteur_shared::llm::{ContextManager, SystemFragment};

static SEQ: AtomicU32 = AtomicU32::new(0);

fn temp_root(tag: &str) -> PathBuf {
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir()
        .join(format!("metteur-harness-stability-{tag}-{}-{seq}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn render(fragments: &[SystemFragment]) -> String {
    let context = ContextManager {
        system_fragments: fragments.to_vec(),
        ..Default::default()
    };
    context.system_text()
}

#[test]
fn assembling_twice_produces_identical_bytes() {
    let root = temp_root("repeat");
    let facts = EnvFacts::new(root.clone(), "2026-01-02");
    let config = Config::default();
    assert_eq!(
        render(&HarnessPrompt::fragments_for(&facts, &config)),
        render(&HarnessPrompt::fragments_for(&facts, &config)),
    );
}

#[test]
fn fragment_order_does_not_depend_on_input_order() {
    let root = temp_root("order-insensitive");
    let facts = EnvFacts::new(root.clone(), "2026-01-02");
    let config = Config::default();
    let mut first = HarnessPrompt::fragments_for(&facts, &config);
    let expected = render(&first);

    // A caller that appends fragments in another order (an addon, a node) must
    // not be able to change the harness half of the prompt.
    first.reverse();
    let mut mixed: Vec<SystemFragment> = vec![SystemFragment {
        priority: 55,
        scope: "addon.extra".to_string(),
        content: "addon text".to_string(),
    }];
    mixed.extend(first);
    let context = ContextManager {
        system_fragments: mixed,
        ..Default::default()
    };
    let rendered = context
        .ordered_fragments()
        .into_iter()
        .filter(|f| metteur_harness::is_harness_fragment(f))
        .map(|f| f.content.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");
    assert_eq!(rendered, expected);
}

#[test]
fn a_date_roll_changes_only_the_date_fragment() {
    let root = temp_root("date-roll");
    let config = Config::default();
    let before = HarnessPrompt::fragments_for(&EnvFacts::new(root.clone(), "2026-01-02"), &config);
    let after = HarnessPrompt::fragments_for(&EnvFacts::new(root.clone(), "2026-01-03"), &config);

    let changed: Vec<&str> = before
        .iter()
        .zip(after.iter())
        .filter(|(a, b)| a != b)
        .map(|(a, _)| a.scope.as_str())
        .collect();
    assert_eq!(changed, vec!["harness.env.date"], "only the last fragment may change");

    // Everything before the date fragment is byte-identical, which is what an
    // explicit cache breakpoint placed there would keep alive.
    let stable_before = before.iter().take_while(|f| f.scope != "harness.env.date");
    let stable_after = after.iter().take_while(|f| f.scope != "harness.env.date");
    assert_eq!(
        stable_before.map(|f| f.content.as_str()).collect::<Vec<_>>(),
        stable_after.map(|f| f.content.as_str()).collect::<Vec<_>>(),
    );
}

#[test]
fn a_model_switch_invalidates_only_the_tail() {
    let root = temp_root("model-switch");
    let config = Config::default();
    let before = HarnessPrompt::fragments_for(&EnvFacts::new(root.clone(), "2026-01-02"), &config);
    let after = HarnessPrompt::fragments_for(
        &EnvFacts::new(root.clone(), "2026-01-02").with_model(Some("other".to_string())),
        &config,
    );
    let changed: Vec<&str> = before
        .iter()
        .zip(after.iter())
        .filter(|(a, b)| a != b)
        .map(|(a, _)| a.scope.as_str())
        .collect();
    assert_eq!(changed, vec!["harness.env.date"]);
}

#[test]
fn refresh_is_a_no_op_for_an_unchanged_environment() {
    let root = temp_root("refresh-stable");
    let config = Config::default();
    let facts = EnvFacts::new(root.clone(), "2026-01-02");
    let mut context = ContextManager::new_from_prompt(vec![], "hi");
    let fragments = HarnessPrompt::fragments_for(&facts, &config);
    assert!(HarnessPrompt::refresh(&mut context, fragments.clone()));
    let stored = context.system_fragments.clone();
    let rendered = context.system_text();

    // Three more turns with the same facts: the stored fragments and the
    // rendered text must not move.
    for _ in 0..3 {
        assert!(!HarnessPrompt::refresh(&mut context, fragments.clone()));
        assert_eq!(context.system_fragments, stored);
        assert_eq!(context.system_text(), rendered);
    }
}

#[test]
fn rendering_is_a_total_order_over_equal_priorities() {
    // Two fragments with the same priority must still order deterministically,
    // otherwise a HashMap-backed caller could reshuffle the prompt.
    let fragments = vec![
        SystemFragment {
            priority: 10,
            scope: "harness.b".to_string(),
            content: "second".to_string(),
        },
        SystemFragment {
            priority: 10,
            scope: "harness.a".to_string(),
            content: "first".to_string(),
        },
    ];
    let forward = assembly::render_all(fragments.iter());
    let mut reversed = fragments.clone();
    reversed.reverse();
    assert_eq!(forward, assembly::render_all(reversed.iter()));
    assert!(forward.starts_with("first"));

    // A map with the same content renders the same way.
    let map: HashMap<String, String> = fragments
        .iter()
        .map(|f| (f.scope.clone(), f.content.clone()))
        .collect();
    let from_map: Vec<SystemFragment> = map
        .into_iter()
        .map(|(scope, content)| SystemFragment {
            priority: 10,
            scope,
            content,
        })
        .collect();
    assert_eq!(assembly::render_all(from_map.iter()), forward);
}

#[test]
fn the_prompt_follows_the_writing_standard() {
    // Guards the authoring rules in `README.md` that a reviewer cannot see in a
    // diff of ordinary code:
    // * no section may be gutted — an empty fragment silently drops rules;
    // * the keyword vocabulary must be defined before it is used;
    // * only documented tags may appear, because a tag that looks like routing
    //   metadata changes how models treat the surrounding text.
    let fragments = metteur_harness::sections::static_fragments();
    for fragment in &fragments {
        assert!(
            fragment.content.chars().count() >= 200,
            "section {} is suspiciously short",
            fragment.scope
        );
    }

    let identity = fragments
        .iter()
        .find(|f| f.scope == "harness.identity")
        .expect("identity section");
    for keyword in ["MUST", "NEVER", "SHOULD", "AVOID", "MAY"] {
        assert!(
            identity.content.contains(keyword),
            "the keyword conventions must define {keyword}"
        );
    }

    const DOCUMENTED_TAGS: [&str; 2] = ["<environment>", "<project-instructions"];
    for fragment in &fragments {
        for candidate in fragment.content.split('<').skip(1) {
            let tag = format!("<{}", candidate.split('>').next().unwrap_or_default());
            assert!(
                DOCUMENTED_TAGS.iter().any(|documented| tag.starts_with(documented)),
                "section {} uses an undocumented tag: {tag}",
                fragment.scope
            );
        }
    }
}
