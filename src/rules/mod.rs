//! Rules: declarative data that turns a message into package fields. None
//! ship with the app; they are written in it. See `schema` for what a rule
//! says and `engine` for how it runs.

pub mod draft;
pub mod engine;
pub mod schema;
pub mod share;

use crate::db::rules::{Sample, StoredRule};
use engine::{CompiledRule, Fields, Input, Outcome};

/// How one regression sample fares with a set of rules.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SampleCheck {
    pub sample_id: i64,
    pub rule_id: String,
    /// The sample belongs to the rule being saved.
    pub own: bool,
    pub passes_before: bool,
    pub passes_after: bool,
    /// What the rules make of it now: the winning rule's name and fields,
    /// or an error, or nothing.
    pub got_rule: Option<String>,
    pub got: Fields,
    pub got_error: Option<String>,
}

impl SampleCheck {
    /// A change that should not be saved without confirming: the rule's own
    /// sample now fails, or another rule's sample that passed now doesn't.
    pub fn is_conflict(&self) -> bool {
        !self.passes_after && (self.own || self.passes_before)
    }
}

fn run<'a>(rules: &'a [CompiledRule], sample: &Sample) -> (Option<&'a CompiledRule>, Outcome) {
    let outcome = engine::evaluate(
        rules,
        &Input {
            channel: sample.channel,
            sender: sample.sender.as_deref(),
            text: &sample.text,
            sent_at: sample.created_at,
        },
    );
    (outcome.rule().map(|i| &rules[i]), outcome)
}

fn passes(rules: &[CompiledRule], sample: &Sample) -> bool {
    match run(rules, sample) {
        (Some(r), Outcome::Matched { fields, .. }) => {
            r.id == sample.rule_id && fields == sample.expected
        }
        _ => false,
    }
}

/// The enabled rules, with `candidate` in place of its stored version.
pub fn with_candidate(
    stored: &[StoredRule],
    candidate: CompiledRule,
) -> (Vec<CompiledRule>, Vec<CompiledRule>) {
    let compile = |r: &StoredRule| engine::compile(&r.id, r.version, r.def.clone()).ok();
    let before: Vec<CompiledRule> = stored
        .iter()
        .filter(|r| r.enabled)
        .filter_map(compile)
        .collect();
    let mut after: Vec<CompiledRule> = stored
        .iter()
        .filter(|r| r.enabled && r.id != candidate.id)
        .filter_map(compile)
        .collect();
    after.push(candidate);
    (before, after)
}

/// Re-runs every sample with the rules before and after a change.
pub fn check_samples(
    before: &[CompiledRule],
    after: &[CompiledRule],
    changed_rule: &str,
    samples: &[Sample],
) -> Vec<SampleCheck> {
    samples
        .iter()
        .map(|s| {
            let (rule, outcome) = run(after, s);
            let (got, got_error) = match outcome {
                Outcome::Matched { fields, .. } => (fields, None),
                Outcome::Failed { fields, error, .. } => (fields, Some(error)),
                Outcome::Unmatched => (Fields::new(), None),
            };
            SampleCheck {
                sample_id: s.id,
                rule_id: s.rule_id.clone(),
                own: s.rule_id == changed_rule,
                passes_before: passes(before, s),
                passes_after: passes(after, s),
                got_rule: rule.map(|r| r.def.name.clone()),
                got,
                got_error,
            }
        })
        .collect()
}
