//! Sharing rules: the self-contained `rule/v1` document, as YAML, as a
//! one-line blob for chat apps, or as a `.lastmile.yaml` file.
//!
//! A shared rule is **untrusted input**: data, never code. It is held to the
//! fixed schema, Rust `regex` (linear time) with size limits, and caps on
//! sizes and counts, and is only ever imported disabled.

use std::io::{Read, Write};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use serde::{Deserialize, Serialize};
use serde_yaml::{Mapping, Value};

use super::engine::{self, CompiledRule, Fields, Input, Outcome};
use super::schema::RuleDef;
use crate::db::rules::{Sample, StoredRule};
use crate::model::Channel;
use crate::redact::{Household, Options, redact, redact_known};

pub const FORMAT: &str = "rule/v1";
pub const BLOB_PREFIX: &str = "lastmile:rule:v1:";

/// Biggest document accepted, before and after decompression.
pub const MAX_DOC: usize = 128 * 1024;
pub const MAX_SAMPLES: usize = 20;
pub const MAX_SAMPLE_TEXT: usize = 8 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SampleDoc {
    pub text: String,
    #[serde(default = "default_channel")]
    pub channel: Channel,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender: Option<String>,
    pub expected: Fields,
}

fn default_channel() -> Channel {
    Channel::Share
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuleDoc {
    /// The same across copies, so importing again updates the rule instead
    /// of duplicating it.
    pub id: String,
    pub version: i64,
    pub def: RuleDef,
    /// Redacted examples, with the fields the rule reads from each.
    pub samples: Vec<SampleDoc>,
}

impl RuleDoc {
    pub fn to_yaml(&self) -> String {
        let mut map = Mapping::new();
        map.insert("lastmile".into(), FORMAT.into());
        map.insert("id".into(), self.id.clone().into());
        map.insert("version".into(), self.version.into());
        if let Value::Mapping(def) = serde_yaml::to_value(&self.def).expect("rules serialise") {
            for (k, v) in def {
                map.insert(k, v);
            }
        }
        if !self.samples.is_empty() {
            map.insert(
                "samples".into(),
                serde_yaml::to_value(&self.samples).expect("samples serialise"),
            );
        }
        serde_yaml::to_string(&Value::Mapping(map)).expect("documents serialise")
    }

    /// The YAML gzipped and base64url-encoded on one line, for chat apps
    /// that mangle multi-line text.
    pub fn to_blob(&self) -> String {
        let mut gz = GzEncoder::new(Vec::new(), Compression::best());
        gz.write_all(self.to_yaml().as_bytes())
            .expect("writing to memory");
        format!(
            "{BLOB_PREFIX}{}",
            URL_SAFE_NO_PAD.encode(gz.finish().expect("writing to memory"))
        )
    }
}

fn gunzip(bytes: &[u8]) -> Result<String, String> {
    let mut out = Vec::new();
    GzDecoder::new(bytes)
        .take(MAX_DOC as u64 + 1)
        .read_to_end(&mut out)
        .map_err(|_| "the blob is not valid (it does not unpack)".to_string())?;
    if out.len() > MAX_DOC {
        return Err(format!("the rule is larger than {} KiB", MAX_DOC / 1024));
    }
    String::from_utf8(out).map_err(|_| "the blob does not hold text".to_string())
}

/// Reads a document from its YAML or its one-line blob. Nothing about it is
/// trusted yet: see [`check`].
pub fn parse(input: &str) -> Result<RuleDoc, String> {
    let input = input.trim().trim_start_matches('\u{feff}');
    if input.is_empty() {
        return Err("nothing to import".into());
    }
    if input.len() > MAX_DOC * 2 {
        return Err(format!("larger than {} KiB", MAX_DOC / 1024));
    }
    let yaml = match input.strip_prefix(BLOB_PREFIX) {
        Some(blob) => {
            let blob: String = blob.split_whitespace().collect();
            let bytes = URL_SAFE_NO_PAD
                .decode(blob.trim_end_matches('='))
                .map_err(|_| "the blob is not valid (bad encoding): copy it whole".to_string())?;
            gunzip(&bytes)?
        }
        None if input.len() > MAX_DOC => {
            return Err(format!("the rule is larger than {} KiB", MAX_DOC / 1024));
        }
        None => input.to_string(),
    };
    let Value::Mapping(mut map) =
        serde_yaml::from_str::<Value>(&yaml).map_err(|e| format!("not valid YAML: {e}"))?
    else {
        return Err("a rule is a YAML mapping (lastmile: rule/v1, …)".into());
    };
    match map.remove("lastmile") {
        Some(Value::String(f)) if f == FORMAT => {}
        Some(Value::String(f)) => {
            return Err(format!(
                "unsupported format {f:?}: this version reads {FORMAT}"
            ));
        }
        _ => return Err(format!("not a lastmile rule: no `lastmile: {FORMAT}` line")),
    }
    let id = match map.remove("id") {
        Some(Value::String(id)) => uuid::Uuid::parse_str(id.trim())
            .map_err(|_| "id: must be a UUID".to_string())?
            .to_string(),
        _ => return Err("id: a UUID is required".into()),
    };
    let version = match map.remove("version") {
        None => 1,
        Some(Value::Number(n)) => n
            .as_i64()
            .filter(|v| *v >= 1)
            .ok_or("version: must be a positive integer")?,
        Some(_) => return Err("version: must be a positive integer".into()),
    };
    let samples: Vec<SampleDoc> = match map.remove("samples") {
        None | Some(Value::Null) => Vec::new(),
        Some(v) => serde_yaml::from_value(v).map_err(|e| format!("samples: {e}"))?,
    };
    let def: RuleDef = serde_yaml::from_value(Value::Mapping(map)).map_err(|e| e.to_string())?;
    Ok(RuleDoc {
        id,
        version,
        def,
        samples,
    })
}

/// How one sample of an imported rule fares.
#[derive(Debug)]
pub struct SampleResult {
    pub text: String,
    pub ok: bool,
    pub expected: Fields,
    pub got: Fields,
    pub error: Option<String>,
}

/// What importing a document would do, with everything it claims checked.
#[derive(Debug)]
pub struct Checked {
    pub compiled: CompiledRule,
    pub samples: Vec<SampleResult>,
}

impl Checked {
    pub fn samples_pass(&self) -> bool {
        self.samples.iter().all(|s| s.ok)
    }
}

/// Validates a parsed document: its limits, that its regexes compile within
/// bounds, and that each sample produces the fields it says.
pub fn check(doc: &RuleDoc) -> Result<Checked, Vec<String>> {
    let mut errors = Vec::new();
    if doc.samples.len() > MAX_SAMPLES {
        errors.push(format!(
            "at most {MAX_SAMPLES} samples, this has {}",
            doc.samples.len()
        ));
    }
    if doc.samples.iter().any(|s| s.text.len() > MAX_SAMPLE_TEXT) {
        errors.push(format!(
            "a sample is longer than {} KiB",
            MAX_SAMPLE_TEXT / 1024
        ));
    }
    let compiled = match engine::compile(&doc.id, doc.version, doc.def.clone()) {
        Ok(c) => Some(c),
        Err(e) => {
            errors.extend(e);
            None
        }
    };
    let Some(compiled) = compiled.filter(|_| errors.is_empty()) else {
        return Err(errors);
    };
    let sent_at = jiff::Timestamp::now();
    let samples = doc
        .samples
        .iter()
        .map(|s| {
            let outcome = engine::evaluate(
                std::slice::from_ref(&compiled),
                &Input {
                    channel: s.channel,
                    sender: s.sender.as_deref(),
                    text: &s.text,
                    sent_at,
                },
            );
            let (got, error) = match outcome {
                Outcome::Matched { fields, .. } => (fields, None),
                Outcome::Failed { fields, error, .. } => (fields, Some(error)),
                Outcome::Unmatched => (Fields::new(), Some("the rule does not match".into())),
            };
            SampleResult {
                text: s.text.clone(),
                ok: error.is_none() && got == s.expected,
                expected: s.expected.clone(),
                got,
                error,
            }
        })
        .collect();
    Ok(Checked { compiled, samples })
}

/// The document to share for a stored rule. Its samples leave the app only
/// redacted, and only those the rule still reads the same way once redacted
/// (so every sample in the document passes when imported): the second
/// number is how many were left out.
pub fn export(rule: &StoredRule, samples: &[Sample], household: &Household) -> (RuleDoc, usize) {
    let compiled = engine::compile(&rule.id, rule.version, rule.def.clone()).ok();
    let mut out = Vec::new();
    let mut dropped = 0;
    for s in samples.iter().filter(|s| s.rule_id == rule.id) {
        let text = redact(&s.text, household, Options::default()).text;
        let sender = s.sender.as_deref().map(|x| redact_known(x, household));
        let fields = compiled.as_ref().and_then(|c| {
            match engine::evaluate(
                std::slice::from_ref(c),
                &Input {
                    channel: s.channel,
                    sender: sender.as_deref(),
                    text: &text,
                    sent_at: s.created_at,
                },
            ) {
                Outcome::Matched { fields, .. } => Some(fields),
                _ => None,
            }
        });
        match fields {
            Some(expected) if out.len() < MAX_SAMPLES => out.push(SampleDoc {
                text,
                channel: s.channel,
                sender,
                expected,
            }),
            _ => dropped += 1,
        }
    }
    (
        RuleDoc {
            id: rule.id.clone(),
            version: rule.version,
            def: rule.def.clone(),
            samples: out,
        },
        dropped,
    )
}

/// A `.lastmile.yaml` file name for a rule.
pub fn file_name(def: &RuleDef) -> String {
    let slug: String = def
        .slug
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    format!("{}.lastmile.yaml", slug.trim_matches('-'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::rules::Origin;

    const RULE: &str = include_str!("../../tests/fixtures/rules/paack-out-for-delivery.yaml");
    const SHARED: &str =
        include_str!("../../tests/fixtures/shared/paack-out-for-delivery.lastmile.yaml");

    fn doc() -> RuleDoc {
        parse(SHARED).unwrap()
    }

    #[test]
    fn the_example_document_is_valid() {
        let d = doc();
        assert_eq!(d.id, "9d2c4b7e-3f51-4a58-8f0e-6c1a2d7b9e40");
        assert_eq!(d.version, 3);
        assert_eq!(d.def.slug, "paack-out-for-delivery");
        assert_eq!(d.samples.len(), 2);
        let checked = check(&d).unwrap();
        assert!(checked.samples_pass(), "{:?}", checked.samples);
    }

    #[test]
    fn yaml_and_blob_round_trip() {
        let d = doc();
        assert_eq!(parse(&d.to_yaml()).unwrap(), d);
        let blob = d.to_blob();
        assert!(
            blob.starts_with(BLOB_PREFIX) && !blob.contains('\n'),
            "{blob}"
        );
        assert_eq!(parse(&blob).unwrap(), d);
        // Chat apps wrap and pad: whitespace inside the blob is ignored.
        let wrapped = format!("  {}\n{}  ", &blob[..40], &blob[40..]);
        assert_eq!(parse(&wrapped).unwrap(), d);
        // The blob is smaller than the text it carries.
        assert!(blob.len() < d.to_yaml().len() * 2);
    }

    #[test]
    fn documents_that_are_not_ours_are_refused() {
        assert!(parse("").is_err());
        assert!(parse("- a\n- b").unwrap_err().contains("mapping"));
        assert!(
            parse("slug: a")
                .unwrap_err()
                .contains("not a lastmile rule")
        );
        assert!(
            parse("lastmile: rule/v2\nid: 9d2c4b7e-3f51-4a58-8f0e-6c1a2d7b9e40")
                .unwrap_err()
                .contains("unsupported format")
        );
        let no_id = SHARED.replace("id: 9d2c4b7e-3f51-4a58-8f0e-6c1a2d7b9e40", "id: nope");
        assert!(parse(&no_id).unwrap_err().contains("UUID"));
        let extra = format!("{SHARED}\nrun: rm -rf /\n");
        assert!(
            parse(&extra).unwrap_err().contains("run"),
            "unknown keys are errors"
        );
        assert!(
            parse(&format!("{BLOB_PREFIX}!!!"))
                .unwrap_err()
                .contains("bad encoding")
        );
        assert!(
            parse(&format!("{BLOB_PREFIX}aGVsbG8"))
                .unwrap_err()
                .contains("does not unpack")
        );
    }

    #[test]
    fn size_limits_hold_even_for_a_compressed_bomb() {
        let mut gz = GzEncoder::new(Vec::new(), Compression::best());
        gz.write_all(&vec![b'a'; 5 * 1024 * 1024]).unwrap();
        let blob = format!(
            "{BLOB_PREFIX}{}",
            URL_SAFE_NO_PAD.encode(gz.finish().unwrap())
        );
        assert!(blob.len() < 10_000, "tiny on the wire");
        assert!(parse(&blob).unwrap_err().contains("larger than"));
        assert!(
            parse(&"a".repeat(MAX_DOC + 1))
                .unwrap_err()
                .contains("larger than")
        );
    }

    #[test]
    fn a_rule_with_bad_regexes_or_too_much_is_refused_by_check() {
        let mut d = doc();
        d.def.matcher.all = vec!["(?=x)".into()];
        assert!(check(&d).unwrap_err()[0].contains("look-around"));
        let mut d = doc();
        d.samples = (0..=MAX_SAMPLES).map(|_| d.samples[0].clone()).collect();
        assert!(check(&d).unwrap_err()[0].contains("at most"));
        let mut d = doc();
        d.samples[0].text = "x".repeat(MAX_SAMPLE_TEXT + 1);
        assert!(check(&d).unwrap_err()[0].contains("sample is longer"));
        // A regex that would take exponential time elsewhere is just bounded here.
        let mut d = doc();
        d.def.matcher.all = vec!["(a+)+$".into()];
        d.samples.clear();
        assert!(check(&d).is_ok());
    }

    #[test]
    fn a_wrong_sample_is_reported() {
        let mut d = doc();
        d.samples[0].expected.insert("code".into(), "0000".into());
        let checked = check(&d).unwrap();
        assert!(!checked.samples_pass());
        assert!(!checked.samples[0].ok && checked.samples[1].ok);
        assert_eq!(checked.samples[0].got["code"], "7315");
    }

    #[test]
    fn export_redacts_samples_and_keeps_only_those_that_still_pass() {
        let def = RuleDef::from_yaml(RULE).unwrap();
        let rule = StoredRule {
            id: "9d2c4b7e-3f51-4a58-8f0e-6c1a2d7b9e40".into(),
            version: 3,
            enabled: true,
            source: RULE.into(),
            def,
            origin: Origin::Local,
            created_by: None,
            created_at: "2026-10-01T00:00:00Z".parse().unwrap(),
            updated_at: "2026-10-01T00:00:00Z".parse().unwrap(),
        };
        let sample = |id, text: &str| {
            let compiled = engine::compile("x", 1, rule.def.clone()).unwrap();
            let Outcome::Matched { fields, .. } = engine::evaluate(
                std::slice::from_ref(&compiled),
                &Input {
                    channel: Channel::Share,
                    sender: None,
                    text,
                    sent_at: "2026-10-18T12:00:00Z".parse().unwrap(),
                },
            ) else {
                panic!("{text}")
            };
            Sample {
                id,
                rule_id: rule.id.clone(),
                message_id: None,
                channel: Channel::Share,
                sender: None,
                text: text.into(),
                expected: fields,
                created_at: "2026-10-18T12:00:00Z".parse().unwrap(),
            }
        };
        let household = Household {
            names: vec!["Ana Ferreira".into()],
            phones: vec!["912345678".into()],
            ..Default::default()
        };
        let samples = [
            sample(
                1,
                "Paack: Olá Ana Ferreira, a sua encomenda PCK7Q2W9D será entregue hoje. Código de entrega 5527. Dúvidas: 912 345 678.",
            ),
            // Another rule's sample is not exported with this one.
            Sample {
                rule_id: "other".into(),
                ..sample(2, "Paack: encomenda PCK1AAAA1 hoje, código 1111")
            },
        ];
        let (doc, dropped) = export(&rule, &samples, &household);
        assert_eq!(dropped, 0);
        assert_eq!(doc.samples.len(), 1);
        let text = &doc.samples[0].text;
        assert!(
            text.contains("‹NAME_1›") && text.contains("‹PHONE_1›"),
            "{text}"
        );
        assert!(
            !text.contains("Ferreira") && !text.contains("912"),
            "{text}"
        );
        assert_eq!(doc.samples[0].expected["tracking_number"], "PCK7Q2W9D");
        // What is exported imports cleanly.
        let again = parse(&doc.to_blob()).unwrap();
        assert!(check(&again).unwrap().samples_pass());
        assert_eq!(file_name(&doc.def), "paack-out-for-delivery.lastmile.yaml");
    }
}
