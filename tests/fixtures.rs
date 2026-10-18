//! Every synthetic message in `tests/fixtures/messages/` run through the
//! rules in `tests/fixtures/rules/`, against its `*.expected.json`. The
//! fixtures are made up in the style of CTT and Paack: no real personal data.

use std::fs;
use std::path::{Path, PathBuf};

use lastmile::ingest::eml;
use lastmile::model::Channel;
use lastmile::rules::engine::{self, CompiledRule, Fields, Input, Outcome};
use lastmile::rules::schema::RuleDef;
use serde::Deserialize;

#[derive(Deserialize)]
struct Expected {
    channel: Channel,
    /// The slug of the rule that should take it, or null.
    rule: Option<String>,
    /// A substring of the error, if the rule should fail.
    #[serde(default)]
    error: Option<String>,
    fields: Fields,
}

fn dir(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn rules() -> Vec<CompiledRule> {
    let mut paths: Vec<PathBuf> = fs::read_dir(dir("rules"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    paths.sort();
    paths
        .iter()
        .map(|p| {
            let def = RuleDef::from_yaml(&fs::read_to_string(p).unwrap())
                .unwrap_or_else(|e| panic!("{}: {e}", p.display()));
            let slug = def.slug.clone();
            engine::compile(&slug, 1, def).unwrap_or_else(|e| panic!("{}: {e:?}", p.display()))
        })
        .collect()
}

#[test]
fn every_fixture_gives_its_expected_fields() {
    let rules = rules();
    assert_eq!(rules.len(), 4);
    let sent_at = "2026-10-18T12:00:00Z".parse().unwrap();
    let mut checked = 0;
    for entry in fs::read_dir(dir("messages")).unwrap() {
        let path = entry.unwrap().path();
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if ext != "txt" && ext != "eml" {
            continue;
        }
        let name = path.file_stem().unwrap().to_str().unwrap();
        let expected: Expected = serde_json::from_str(
            &fs::read_to_string(dir("messages").join(format!("{name}.expected.json"))).unwrap(),
        )
        .unwrap();
        let raw = fs::read(&path).unwrap();
        let (sender, text) = if ext == "eml" {
            let email = eml::parse(&raw).unwrap();
            let text = format!("{}\n\n{}", email.subject.unwrap_or_default(), email.text);
            (email.sender, text)
        } else {
            (None, String::from_utf8(raw).unwrap())
        };
        let outcome = engine::evaluate(
            &rules,
            &Input {
                channel: expected.channel,
                sender: sender.as_deref(),
                text: &text,
                sent_at,
            },
        );
        let rule = outcome.rule().map(|i| rules[i].id.clone());
        assert_eq!(rule, expected.rule, "{name}: wrong rule ({outcome:?})");
        match outcome {
            Outcome::Matched { fields, .. } => {
                assert_eq!(expected.error, None, "{name}: expected an error");
                assert_eq!(fields, expected.fields, "{name}");
            }
            Outcome::Failed { error, fields, .. } => {
                let want = expected.error.unwrap_or_else(|| panic!("{name}: {error}"));
                assert!(error.contains(&want), "{name}: {error}");
                assert_eq!(fields, expected.fields, "{name}");
            }
            Outcome::Unmatched => assert!(expected.fields.is_empty(), "{name}"),
        }
        checked += 1;
    }
    assert_eq!(checked, 8);
}
