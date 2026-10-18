//! Drafting a rule with an LLM, from one message no rule matched.
//!
//! Only the text an admin has read and sent goes out. The draft is a JSON
//! object (strict schema where the server supports it), converted to a
//! [`RuleDef`] and then *validated locally* before anyone sees it: it must
//! compile, match the message and not change what other rules' samples
//! read. A draft that fails goes back to the model with the problems, up to
//! [`MAX_RETRIES`] times.

use std::collections::BTreeMap;

use jiff::Timestamp;
use serde::Deserialize;
use serde_json::{Value, json};

use super::engine::{self, Input, Outcome};
use super::schema::{Field, Match, RuleDef, Set};
use super::{check_samples, with_candidate};
use crate::db::rules::{Sample, StoredRule};
use crate::llm::{Client, Message, Role};
use crate::model::{Channel, CodeKind, Status};

/// Automatic retries after the first answer.
pub const MAX_RETRIES: usize = 3;
const SCHEMA_NAME: &str = "lastmile_rule_draft";

#[derive(Debug, Deserialize)]
struct DraftJson {
    explanation: String,
    slug: String,
    name: String,
    #[serde(default)]
    courier: Option<String>,
    #[serde(default)]
    channels: Vec<Channel>,
    #[serde(default)]
    sender: Vec<String>,
    #[serde(default)]
    all: Vec<String>,
    #[serde(default)]
    none: Vec<String>,
    #[serde(default)]
    extract: Vec<ExtractJson>,
    #[serde(default)]
    status: Option<Status>,
    #[serde(default)]
    code_kind: Option<CodeKind>,
    #[serde(default)]
    priority: Option<i64>,
    #[serde(default)]
    follow_up: bool,
}

#[derive(Debug, Deserialize)]
struct ExtractJson {
    field: Field,
    pattern: String,
}

/// The JSON Schema of the model's reply: strict-mode friendly (every
/// property required, nulls for the optional ones, no maps).
pub fn schema() -> Value {
    let strings = json!({"type": "array", "items": {"type": "string"}});
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": [
            "explanation", "slug", "name", "courier", "channels", "sender", "all",
            "none", "extract", "status", "code_kind", "priority", "follow_up"
        ],
        "properties": {
            "explanation": {"type": "string", "description": "One to three sentences: what the rule reads and why it is written this way."},
            "slug": {"type": "string", "description": "Short lowercase name with dashes, e.g. ctt-ready-for-pickup."},
            "name": {"type": "string", "description": "Human name, e.g. \"CTT – ready for pickup (SMS)\"."},
            "courier": {"type": ["string", "null"], "description": "The courier, as people write it: CTT, Paack…"},
            "channels": {"type": "array", "items": {"type": "string", "enum": ["upload", "share", "api", "paste"]}},
            "sender": strings,
            "all": strings,
            "none": strings,
            "extract": {"type": "array", "items": {
                "type": "object",
                "additionalProperties": false,
                "required": ["field", "pattern"],
                "properties": {
                    "field": {"type": "string", "enum": Field::ALL.iter().map(|f| f.as_str()).collect::<Vec<_>>()},
                    "pattern": {"type": "string", "description": "Rust regex with a named group (?P<v>…) holding the value."}
                }
            }},
            "status": {"type": ["string", "null"], "enum": [
                "announced", "in_transit", "out_for_delivery", "ready_for_pickup", "delivered", "exception", null
            ]},
            "code_kind": {"type": ["string", "null"], "enum": ["delivery", "pickup", null]},
            "priority": {"type": "integer"},
            "follow_up": {"type": "boolean"}
        }
    })
}

fn slugify(s: &str) -> String {
    let mut out = String::new();
    for c in s.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    out.trim_matches('-').to_string()
}

fn non_empty(s: Option<String>) -> Option<String> {
    s.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

impl DraftJson {
    fn into_def(self) -> (RuleDef, String) {
        let mut extract: BTreeMap<Field, Option<String>> = BTreeMap::new();
        for e in self.extract {
            if !e.pattern.trim().is_empty() {
                extract.insert(e.field, Some(e.pattern));
            }
        }
        let slug = match slugify(&self.slug) {
            s if s.is_empty() => "draft".to_string(),
            s => s,
        };
        let def = RuleDef {
            slug,
            name: match self.name.trim() {
                "" => "Drafted rule".to_string(),
                n => n.to_string(),
            },
            description: None,
            courier: non_empty(self.courier),
            matcher: Match {
                channels: self.channels,
                sender: self
                    .sender
                    .into_iter()
                    .filter(|s| !s.trim().is_empty())
                    .collect(),
                all: self.all.into_iter().filter(|s| !s.is_empty()).collect(),
                none: self.none.into_iter().filter(|s| !s.is_empty()).collect(),
            },
            extract,
            set: Set {
                status: self.status,
                code_kind: self.code_kind,
                merchant: None,
            },
            priority: self.priority.unwrap_or(100),
            follow_up: self.follow_up,
        };
        (def, self.explanation.trim().to_string())
    }
}

/// The JSON object in a reply, which models sometimes wrap in code fences
/// or a sentence.
fn extract_json(text: &str) -> &str {
    let text = text.trim();
    match (text.find('{'), text.rfind('}')) {
        (Some(a), Some(b)) if a < b => &text[a..=b],
        _ => text,
    }
}

const FIELD_HELP: &str = "\
- title: a short name for the package, only if the message gives one
- merchant: the shop it comes from
- tracking_number: the courier's number (e.g. RR123456789PT)
- order_ref: the shop's order number
- tracking_url: a link to follow it
- code: the code to give the courier at the door, or to type at a locker or show at a shop
- code_expires_at: the last day to collect it (a date)
- pickup_point: the name of the locker or shop
- pickup_address: its address
- pickup_hours: its opening hours
- eta: the expected delivery day (a date; dd/mm or dd/mm/yyyy)";

fn system_prompt(client: &Client, existing: &[&RuleDef]) -> String {
    let mut p = String::from(
        "You write rules for lastmile, a household package tracker. A rule reads one kind of \
message (an SMS or WhatsApp text, or an email, from a courier or a shop) and extracts the \
package's details with regular expressions. You are given one message that no rule reads yet.\n\n\
How rules work:\n\
- `all` lists regexes that must all match the message: the phrases that identify this kind of \
message. `none` lists regexes that must not match: use them to exclude other kinds of message \
that share the same words (a return notice, a reminder). Use as few conditions as tell the \
message apart.\n\
- `extract` maps a field to a regex with a named group `(?P<v>...)`; the value is that group. \
Fields:\n",
    );
    p.push_str(FIELD_HELP);
    p.push_str(
        "\n- `status` is what the message says about the package: announced (order confirmed, label \
created), in_transit, out_for_delivery, ready_for_pickup (waiting at a locker or shop, a code is \
needed), delivered, exception (failed attempt, returned, customs). null if the message says \
nothing about it.\n\
- `code_kind` is delivery (the code is given to the courier at the door) or pickup (typed at a \
locker or shown at a shop).\n\
- `channels`: texts are `share`, `api` or `paste`; emails are `upload`, and an email's text starts \
with its subject. Leave it empty to read every channel. `sender` takes patterns like `*@ctt.pt` \
and only matters for emails: texts have no sender, so a rule must work on the content alone.\n\
- Every rule needs a tracking number or an order reference, so extract one. Set `follow_up` true \
only for messages that never name the package (a reminder): a person then attaches it by hand.\n\
- `priority` is 100 by default; use a higher one only for a narrower rule that must win over a \
broader one.\n\n\
Regex dialect: Rust's `regex` crate. There is no look-around and no backreferences. Use `(?i)` \
for case. Messages come with or without accents (ã/a, ç/c, é/e): write `[aã]`-style classes, or \
avoid accented letters. Escape nothing twice: your JSON string is the pattern.\n\n\
The message is one example of its kind: other messages of the same kind have other names, \
addresses, numbers and codes. Write patterns that work for any value in their place (for an \
address, something like `[^.\\n]+`; for a code, its shape such as `\\d{6}`, not the digits you \
see), and never put a person's name, address or phone number in a pattern.\n",
    );
    if !existing.is_empty() {
        p.push_str("\nRules this household already has for the same courier, to follow their style and priorities:\n");
        for def in existing {
            p.push_str("\n```yaml\n");
            p.push_str(&def.to_yaml());
            p.push_str("```\n");
        }
    }
    if client.schema_in_prompt() {
        p.push_str(
            "\nReply with only a JSON object (no code fences, no other text) with exactly these keys and types, \
as in this JSON Schema:\n",
        );
        p.push_str(&schema().to_string());
        p.push('\n');
    }
    p
}

/// What to draft a rule from.
pub struct Request<'a> {
    /// The text of the message the admin reviewed: what is sent.
    pub message: &'a str,
    pub sender: Option<&'a str>,
    pub channel: Channel,
    /// What the admin says it is ("Paack out for delivery, the 4 digits are the code").
    pub hint: &'a str,
    pub stored: &'a [StoredRule],
    pub samples: &'a [Sample],
    pub sent_at: Timestamp,
}

#[derive(Debug, Default)]
pub struct CallStat {
    pub ok: bool,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

#[derive(Debug, Default)]
pub struct Drafted {
    /// The last draft, as YAML: valid when `problems` is empty.
    pub yaml: Option<String>,
    pub explanation: String,
    /// Requests made to the model.
    pub attempts: usize,
    /// What was still wrong with the last draft after the retries.
    pub problems: Vec<String>,
    /// Why no draft was made at all (the model couldn't be reached).
    pub error: Option<String>,
    pub calls: Vec<CallStat>,
}

fn user_message(req: &Request) -> String {
    let mut m = format!("Channel: {}\n", req.channel.as_str());
    if let Some(s) = req.sender {
        m.push_str(&format!("Sender: {s}\n"));
    }
    let hint = req.hint.trim();
    if !hint.is_empty() {
        m.push_str(&format!("What this message is: {hint}\n"));
    }
    m.push_str(&format!("\nMessage:\n<<<\n{}\n>>>\n", req.message.trim()));
    m
}

/// What is wrong with a draft, in words that are safe to send back to the
/// model: they name fields and rules, and never quote a message.
fn problems_with(def: &RuleDef, req: &Request, rule_id: &str) -> Vec<String> {
    let compiled = match engine::compile(rule_id, 0, def.clone()) {
        Ok(c) => c,
        Err(errors) => return errors,
    };
    let outcome = engine::evaluate(
        std::slice::from_ref(&compiled),
        &Input {
            channel: req.channel,
            sender: req.sender,
            text: req.message,
            sent_at: req.sent_at,
        },
    );
    let mut problems = Vec::new();
    match outcome {
        Outcome::Unmatched => {
            problems.push(
                "The rule does not match the message: check `channels`, `sender`, `all` and `none` against it."
                    .into(),
            );
            return problems;
        }
        Outcome::Failed { error, fields, .. } => {
            let found: Vec<&str> = fields.keys().map(String::as_str).collect();
            problems.push(format!(
                "The rule matches but is not usable: {error}. It read these fields: {}.",
                if found.is_empty() {
                    "none".into()
                } else {
                    found.join(", ")
                }
            ));
        }
        Outcome::Matched { .. } => {}
    }
    if problems.is_empty() {
        let (before, after) = with_candidate(req.stored, compiled);
        let names: BTreeMap<&str, &str> = req
            .stored
            .iter()
            .map(|r| (r.id.as_str(), r.def.name.as_str()))
            .collect();
        let mut broken: Vec<&str> = check_samples(&before, &after, rule_id, req.samples)
            .iter()
            .filter(|c| c.is_conflict())
            .filter_map(|c| names.get(c.rule_id.as_str()).copied())
            .collect();
        broken.sort_unstable();
        broken.dedup();
        if !broken.is_empty() {
            problems.push(format!(
                "This rule would change what the existing rules {} read in their saved samples. Make it narrower (more `all` or a `none`) or give it a lower `priority`.",
                broken.join(", ")
            ));
        }
    }
    problems
}

/// Asks the model for a rule and validates the answer, retrying with the
/// problems up to [`MAX_RETRIES`] times. Never fails: what went wrong is in
/// the result.
pub async fn draft(client: &Client, req: &Request<'_>) -> Drafted {
    let rule_id = "draft";
    let courier_rules: Vec<&RuleDef> = req
        .stored
        .iter()
        .filter(|r| {
            // Rules for the same courier, when the admin named one in the hint.
            let hint = req.hint.to_lowercase();
            r.def
                .courier
                .as_ref()
                .is_some_and(|c| hint.contains(&c.to_lowercase()))
        })
        .take(3)
        .map(|r| &r.def)
        .collect();
    let mut messages = vec![
        Message::new(Role::System, system_prompt(client, &courier_rules)),
        Message::new(Role::User, user_message(req)),
    ];
    let mut out = Drafted::default();
    for _ in 0..=MAX_RETRIES {
        out.attempts += 1;
        let completion = match client.complete(&messages, SCHEMA_NAME, &schema()).await {
            Ok(c) => c,
            Err(e) => {
                out.calls.push(CallStat::default());
                out.error = Some(e.to_string());
                return out;
            }
        };
        out.calls.push(CallStat {
            ok: true,
            input_tokens: completion.input_tokens,
            output_tokens: completion.output_tokens,
        });
        let problems = match serde_json::from_str::<DraftJson>(extract_json(&completion.text)) {
            Err(e) => vec![format!(
                "The reply is not a JSON object with the requested keys ({e}). Reply with only that object."
            )],
            Ok(json) => {
                let (def, explanation) = json.into_def();
                out.explanation = explanation;
                out.yaml = Some(def.to_yaml());
                problems_with(&def, req, rule_id)
            }
        };
        if problems.is_empty() {
            out.problems.clear();
            return out;
        }
        messages.push(Message::new(Role::Assistant, completion.text));
        messages.push(Message::new(
            Role::User,
            format!(
                "That rule has problems:\n{}\nFix them and reply with the corrected rule, in the same format.",
                problems.iter().map(|p| format!("- {p}")).collect::<Vec<_>>().join("\n")
            ),
        ));
        out.problems = problems;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::rules::{Origin, StoredRule};
    use clap::Parser;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, Request as WmRequest, Respond, ResponseTemplate};

    const MESSAGE: &str = "Paack: Olá, a sua encomenda PCK7Q2W9D será entregue hoje. Código de entrega 5527. Dúvidas: 912 345 678.";

    fn reply(def: Value) -> Value {
        json!({
            "choices": [{"message": {"content": def.to_string()}}],
            "usage": {"prompt_tokens": 100, "completion_tokens": 40},
        })
    }

    fn good() -> Value {
        json!({
            "explanation": "Paack's out-for-delivery text, with the four digit delivery code.",
            "slug": "Paack – out for delivery!",
            "name": "Paack – out for delivery (SMS)",
            "courier": "Paack",
            "channels": ["share", "paste"],
            "sender": [],
            "all": ["(?i)paack", "(?i)ser[aá] entregue hoje"],
            "none": [],
            "extract": [
                {"field": "tracking_number", "pattern": "(?i)encomenda (?P<v>PCK[A-Z0-9]+)"},
                {"field": "code", "pattern": "(?i)c[oó]digo de entrega (?P<v>\\d{4})"},
            ],
            "status": "out_for_delivery",
            "code_kind": "delivery",
            "priority": 100,
            "follow_up": false,
        })
    }

    /// Answers with each of `answers` in turn, and keeps what it was asked.
    struct Script(std::sync::Mutex<std::vec::IntoIter<ResponseTemplate>>);

    impl Respond for Script {
        fn respond(&self, _: &WmRequest) -> ResponseTemplate {
            self.0
                .lock()
                .unwrap()
                .next()
                .expect("more requests than answers")
        }
    }

    async fn run(answers: Vec<ResponseTemplate>, stored: &[StoredRule]) -> (Drafted, Vec<Value>) {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(Script(std::sync::Mutex::new(answers.into_iter())))
            .mount(&server)
            .await;
        let uri = format!("{}/v1", server.uri());
        let cfg = crate::config::Config::try_parse_from([
            "lastmile",
            "--llm-model",
            "m",
            "--llm-base-url",
            &uri,
        ])
        .unwrap();
        let client = Client::from_config(&cfg.llm).unwrap().unwrap();
        let out = draft(
            &client,
            &Request {
                message: MESSAGE,
                sender: None,
                channel: Channel::Share,
                hint: "Paack out for delivery, the 4 digits are the code",
                stored,
                samples: &[],
                sent_at: "2026-10-18T12:00:00Z".parse().unwrap(),
            },
        )
        .await;
        let sent = server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .map(|r| r.body_json().unwrap())
            .collect();
        (out, sent)
    }

    fn ok(def: Value) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(reply(def))
    }

    #[tokio::test]
    async fn a_good_draft_is_accepted_at_once() {
        let (out, sent) = run(vec![ok(good())], &[]).await;
        assert_eq!(out.attempts, 1);
        assert!(out.problems.is_empty() && out.error.is_none(), "{out:?}");
        assert_eq!(out.calls[0].input_tokens, 100);
        let def = RuleDef::from_yaml(out.yaml.as_deref().unwrap()).unwrap();
        assert_eq!(def.slug, "paack-out-for-delivery");
        assert_eq!(def.set.status, Some(Status::OutForDelivery));
        assert_eq!(
            def.extract[&Field::Code].as_deref(),
            Some("(?i)c[oó]digo de entrega (?P<v>\\d{4})")
        );
        // What was sent is the reviewed text, and says what the admin said.
        let prompt = serde_json::to_string(&sent[0]).unwrap();
        assert!(
            prompt.contains("PCK7Q2W9D") && prompt.contains("912 345 678"),
            "{prompt}"
        );
        assert!(prompt.contains("the 4 digits are the code"));
        assert_eq!(sent[0]["response_format"]["json_schema"]["strict"], true);
    }

    #[tokio::test]
    async fn problems_go_back_to_the_model_without_quoting_the_message() {
        let mut no_id = good();
        no_id["extract"] =
            json!([{"field": "code", "pattern": "(?i)c[oó]digo de entrega (?P<v>\\d{4})"}]);
        let mut broken = good();
        broken["all"] = json!(["(?<=a)paack"]);
        let mut unmatched = good();
        unmatched["all"] = json!(["(?i)ficou no cacifo"]);
        let (out, sent) = run(vec![ok(broken), ok(unmatched), ok(no_id), ok(good())], &[]).await;
        assert_eq!(out.attempts, 4);
        assert!(out.problems.is_empty(), "{out:?}");
        let feedback: Vec<&str> = sent[1..]
            .iter()
            .map(|r| {
                r["messages"].as_array().unwrap().last().unwrap()["content"]
                    .as_str()
                    .unwrap()
            })
            .collect();
        assert!(
            feedback[0].contains("look-around is not supported"),
            "{}",
            feedback[0]
        );
        assert!(
            feedback[1].contains("does not match the message"),
            "{}",
            feedback[1]
        );
        assert!(
            feedback[2].contains("no tracking number"),
            "{}",
            feedback[2]
        );
        // The conversation keeps its earlier turns.
        assert_eq!(sent[3]["messages"].as_array().unwrap().len(), 8);
        for f in feedback {
            assert!(!f.contains("PCK7Q2W9D") && !f.contains("5527"), "{f}");
        }
    }

    #[tokio::test]
    async fn after_the_retries_the_last_draft_and_its_problems_are_returned() {
        let mut bad = good();
        bad["all"] = json!(["(?i)ficou no cacifo"]);
        let answers = (0..=MAX_RETRIES).map(|_| ok(bad.clone())).collect();
        let (out, _) = run(answers, &[]).await;
        assert_eq!(out.attempts, 1 + MAX_RETRIES);
        assert_eq!(out.problems.len(), 1);
        assert!(out.yaml.unwrap().contains("ficou no cacifo"));
    }

    #[tokio::test]
    async fn a_reply_that_is_not_json_is_retried_and_fenced_json_is_read() {
        let fenced = format!("Here you go:\n```json\n{}\n```", good());
        let answers = vec![
            ResponseTemplate::new(200).set_body_json(
                json!({"choices": [{"message": {"content": "I cannot help with that."}}]}),
            ),
            ResponseTemplate::new(200)
                .set_body_json(json!({"choices": [{"message": {"content": fenced}}]})),
        ];
        let (out, _) = run(answers, &[]).await;
        assert_eq!(out.attempts, 2);
        assert!(out.problems.is_empty(), "{out:?}");
    }

    #[tokio::test]
    async fn a_failing_server_is_reported_not_retried() {
        let (out, _) = run(
            vec![ResponseTemplate::new(500).set_body_string("boom")],
            &[],
        )
        .await;
        assert_eq!(out.attempts, 1);
        assert!(out.yaml.is_none());
        assert!(out.error.unwrap().contains("500"), "status in the error");
    }

    #[tokio::test]
    async fn a_draft_that_breaks_another_rules_samples_is_sent_back() {
        // An existing rule with a sample the new one (priority 500) would take.
        let existing_def = RuleDef::from_yaml(
            "slug: other\nname: Other\ncourier: Paack\nmatch: {all: ['(?i)paack', 'PCK']}\nextract: {tracking_number: '(?P<v>PCK[A-Z0-9]+)'}\n",
        )
        .unwrap();
        let stored = StoredRule {
            id: "other-id".into(),
            version: 1,
            enabled: true,
            source: existing_def.to_yaml(),
            def: existing_def,
            origin: Origin::Local,
            created_by: None,
            created_at: "2026-10-01T00:00:00Z".parse().unwrap(),
            updated_at: "2026-10-01T00:00:00Z".parse().unwrap(),
        };
        let sample_text = "Paack: encomenda PCK1AAAA1 a caminho";
        let mut expected = crate::rules::engine::Fields::new();
        expected.insert("courier".into(), "Paack".into());
        expected.insert("tracking_number".into(), "PCK1AAAA1".into());
        let sample = Sample {
            id: 1,
            rule_id: "other-id".into(),
            message_id: None,
            channel: Channel::Share,
            sender: None,
            text: sample_text.into(),
            expected,
            created_at: "2026-10-01T00:00:00Z".parse().unwrap(),
        };
        let mut greedy = good();
        greedy["priority"] = json!(500);
        greedy["all"] = json!(["(?i)paack"]);
        let def = serde_json::from_value::<DraftJson>(greedy)
            .unwrap()
            .into_def()
            .0;
        let samples = [sample];
        let stored = [stored];
        let problems = problems_with(
            &def,
            &Request {
                message: MESSAGE,
                sender: None,
                channel: Channel::Share,
                hint: "",
                stored: &stored,
                samples: &samples,
                sent_at: "2026-10-18T12:00:00Z".parse().unwrap(),
            },
            "draft",
        );
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("Other"), "{problems:?}");
    }

    #[test]
    fn the_schema_is_strict_mode_friendly() {
        let s = schema();
        let props = s["properties"].as_object().unwrap();
        let required: Vec<&str> = s["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(props.len(), required.len(), "every property is required");
        assert!(required.iter().all(|r| props.contains_key(*r)));
        assert_eq!(s["additionalProperties"], false);
        assert_eq!(
            s["properties"]["extract"]["items"]["additionalProperties"],
            false
        );
        assert_eq!(
            s["properties"]["extract"]["items"]["properties"]["field"]["enum"]
                .as_array()
                .unwrap()
                .len(),
            Field::ALL.len()
        );
    }
}
