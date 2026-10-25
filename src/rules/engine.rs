//! Running rules on a message: normalise the text, find the rule that
//! matches best, extract its fields.

use std::collections::BTreeMap;

use jiff::Timestamp;
use jiff::civil::Date;
use regex::{Regex, RegexBuilder};
use unicode_normalization::UnicodeNormalization;

use super::schema::{Field, RuleDef};
use crate::model::Channel;

/// What a rule extracted: field names (and `status`, `code_kind`,
/// `courier` from the rule itself) to values. Dates are `YYYY-MM-DD`.
pub type Fields = BTreeMap<String, String>;

/// Limits that keep imported or pasted rules cheap to run.
pub const MAX_PATTERNS: usize = 32;
pub const MAX_PATTERN_LEN: usize = 1000;
const REGEX_SIZE_LIMIT: usize = 1 << 20;

/// Normalises text before rules see it: NFC, `\n` line ends, no
/// non-breaking spaces, runs of spaces collapsed, lines trimmed, at most one
/// blank line in a row.
pub fn normalize(text: &str) -> String {
    let text: String = text
        .nfc()
        .map(|c| match c {
            '\u{a0}' | '\u{2007}' | '\u{202f}' | '\t' => ' ',
            c => c,
        })
        .collect();
    let mut out = String::with_capacity(text.len());
    let mut blank = 0;
    for line in text.replace("\r\n", "\n").replace('\r', "\n").lines() {
        let line = line
            .split(' ')
            .filter(|w| !w.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        if line.is_empty() {
            blank += 1;
            if blank > 1 || out.is_empty() {
                continue;
            }
        } else {
            blank = 0;
        }
        out.push_str(&line);
        out.push('\n');
    }
    out.trim_end().to_string()
}

pub struct Input<'a> {
    pub channel: Channel,
    pub sender: Option<&'a str>,
    /// Subject and body, or the shared text. Normalised by [`evaluate`].
    pub text: &'a str,
    /// For dates written without a year.
    pub sent_at: Timestamp,
}

#[derive(Debug)]
pub struct CompiledRule {
    pub id: String,
    pub version: i64,
    pub def: RuleDef,
    all: Vec<Regex>,
    none: Vec<Regex>,
    extract: Vec<(Field, Regex)>,
}

fn build(pattern: &str) -> Result<Regex, String> {
    if pattern.len() > MAX_PATTERN_LEN {
        return Err(format!("longer than {MAX_PATTERN_LEN} characters"));
    }
    RegexBuilder::new(pattern)
        .size_limit(REGEX_SIZE_LIMIT)
        .dfa_size_limit(REGEX_SIZE_LIMIT)
        .build()
        .map_err(|e| match e {
            regex::Error::Syntax(s) => {
                let hint = if pattern.contains("(?=")
                    || pattern.contains("(?!")
                    || pattern.contains("(?<=")
                    || pattern.contains("(?<!")
                {
                    " (look-around is not supported)"
                } else {
                    ""
                };
                format!("{}{hint}", s.lines().last().unwrap_or(&s).trim())
            }
            regex::Error::CompiledTooBig(_) => "the pattern is too big".into(),
            e => e.to_string(),
        })
}

/// Compiles a rule, or lists everything wrong with it.
pub fn compile(id: &str, version: i64, def: RuleDef) -> Result<CompiledRule, Vec<String>> {
    let mut errors = Vec::new();
    if def.slug.trim().is_empty() {
        errors.push("slug: must not be empty".to_string());
    }
    if def.name.trim().is_empty() {
        errors.push("name: must not be empty".to_string());
    }
    let count = def.matcher.all.len()
        + def.matcher.none.len()
        + def.matcher.sender.len()
        + def.extract.len();
    if count > MAX_PATTERNS {
        errors.push(format!(
            "at most {MAX_PATTERNS} patterns, this rule has {count}"
        ));
    }
    if def.matcher.all.is_empty() && def.matcher.sender.is_empty() {
        errors.push("match: needs at least one `all` regex or a `sender`".into());
    }
    let mut compile_list = |key: &str, list: &[String]| -> Vec<Regex> {
        list.iter()
            .enumerate()
            .filter_map(|(i, p)| match build(p) {
                Ok(re) => Some(re),
                Err(e) => {
                    errors.push(format!("match.{key}[{i}]: {e}"));
                    None
                }
            })
            .collect()
    };
    let all = compile_list("all", &def.matcher.all);
    let none = compile_list("none", &def.matcher.none);
    let mut extract = Vec::new();
    for (field, pattern) in &def.extract {
        let Some(pattern) = pattern else { continue };
        match build(pattern) {
            Ok(re) if re.capture_names().any(|n| n == Some("v")) => extract.push((*field, re)),
            Ok(_) => errors.push(format!(
                "extract.{}: needs a named group (?P<v>…) for the value",
                field.as_str()
            )),
            Err(e) => errors.push(format!("extract.{}: {e}", field.as_str())),
        }
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    Ok(CompiledRule {
        id: id.to_string(),
        version,
        def,
        all,
        none,
        extract,
    })
}

/// `*@ctt.pt`-style patterns, ignoring case.
fn glob_matches(pattern: &str, value: &str) -> bool {
    let (pattern, value) = (pattern.to_lowercase(), value.to_lowercase());
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == value;
    }
    let mut rest = value.as_str();
    for (i, part) in parts.iter().enumerate() {
        if i == 0 {
            match rest.strip_prefix(part) {
                Some(r) => rest = r,
                None => return false,
            }
        } else if i == parts.len() - 1 {
            return rest.ends_with(part);
        } else {
            match rest.find(part) {
                Some(at) => rest = &rest[at + part.len()..],
                None => return false,
            }
        }
    }
    true
}

impl CompiledRule {
    /// Whether the rule's conditions hold (extraction aside).
    pub fn matches(&self, channel: Channel, sender: Option<&str>, text: &str) -> bool {
        let m = &self.def.matcher;
        if !m.channels.is_empty() && !m.channels.contains(&channel) {
            return false;
        }
        if let Some(sender) = sender
            && !m.sender.is_empty()
            && !m.sender.iter().any(|p| glob_matches(p, sender))
        {
            return false;
        }
        self.all.iter().all(|re| re.is_match(text)) && !self.none.iter().any(|re| re.is_match(text))
    }

    fn specificity(&self) -> usize {
        let m = &self.def.matcher;
        m.all.len() + m.none.len() + m.sender.len() + usize::from(!m.channels.is_empty())
    }

    /// The fields this rule reads from `text`, or why it couldn't.
    pub fn extract(&self, text: &str, sent_at: Timestamp) -> Result<Fields, (String, Fields)> {
        let mut fields = Fields::new();
        let mut problems = Vec::new();
        for (field, re) in &self.extract {
            let Some(value) = re.captures(text).and_then(|c| c.name("v")) else {
                continue;
            };
            let value = value
                .as_str()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            if value.is_empty() {
                continue;
            }
            if field.is_date() {
                match parse_date(&value, sent_at) {
                    Some(d) => {
                        fields.insert(field.as_str().into(), d.to_string());
                    }
                    None => problems.push(format!(
                        "{}: can't read {value:?} as a date",
                        field.as_str()
                    )),
                }
            } else {
                fields.insert(field.as_str().into(), value);
            }
        }
        let set = &self.def.set;
        if let Some(status) = set.status {
            fields.insert("status".into(), status.as_str().into());
        }
        if let Some(kind) = set.code_kind {
            fields.insert("code_kind".into(), kind.as_str().into());
        }
        if let Some(merchant) = &set.merchant {
            fields
                .entry("merchant".into())
                .or_insert_with(|| merchant.clone());
        }
        if let Some(courier) = &self.def.courier {
            fields.insert("courier".into(), courier.clone());
        }
        let named = fields.contains_key("tracking_number") || fields.contains_key("order_ref");
        if !named && !self.def.follow_up {
            problems.push(
                "found no tracking number or order reference (set followUp: true for messages that don't name the package)"
                    .into(),
            );
        }
        if problems.is_empty() {
            Ok(fields)
        } else {
            Err((problems.join("; "), fields))
        }
    }
}

/// Reads `2026-10-21`, `21/10/2026`, `21-10-2026`, `21.10.2026` or `21/10`
/// (the year the message was sent; the next one if that day is long past),
/// or `hoje`, `amanhã` and `ontem` relative to the day the message was sent.
pub fn parse_date(value: &str, sent_at: Timestamp) -> Option<Date> {
    let value = value.trim();
    if let Ok(d) = value.parse::<Date>() {
        return Some(d);
    }
    let sent = sent_at.to_zoned(jiff::tz::TimeZone::UTC).date();
    let days = match value.to_lowercase().as_str() {
        "hoje" => Some(0),
        "amanhã" | "amanha" => Some(1),
        "ontem" => Some(-1),
        _ => None,
    };
    if let Some(days) = days {
        return sent.checked_add(jiff::Span::new().days(days)).ok();
    }
    let parts: Vec<&str> = value.split(['/', '-', '.']).collect();
    let num = |s: &str| s.parse::<i16>().ok();
    match parts.as_slice() {
        [d, m, y] => {
            let y = num(y)?;
            let y = if y < 100 { 2000 + y } else { y };
            Date::new(y, num(m)? as i8, num(d)? as i8).ok()
        }
        [d, m] => {
            let date = Date::new(sent.year(), num(m)? as i8, num(d)? as i8).ok()?;
            if (sent - date).get_days() > 180 {
                Date::new(sent.year() + 1, date.month(), date.day()).ok()
            } else {
                Some(date)
            }
        }
        _ => None,
    }
}

/// The result of running every rule on a message.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Index of the winning rule and what it extracted.
    Matched {
        rule: usize,
        fields: Fields,
    },
    /// The winning rule matched but its fields didn't come out.
    Failed {
        rule: usize,
        error: String,
        fields: Fields,
    },
    Unmatched,
}

impl Outcome {
    pub fn rule(&self) -> Option<usize> {
        match self {
            Outcome::Matched { rule, .. } | Outcome::Failed { rule, .. } => Some(*rule),
            Outcome::Unmatched => None,
        }
    }
}

/// Tries every rule; the highest priority wins, then the most specific,
/// then the first by name (so the result never depends on storage order).
pub fn evaluate(rules: &[CompiledRule], input: &Input) -> Outcome {
    let text = normalize(input.text);
    let winner = rules
        .iter()
        .enumerate()
        .filter(|(_, r)| r.matches(input.channel, input.sender, &text))
        .max_by(|(_, a), (_, b)| {
            a.def
                .priority
                .cmp(&b.def.priority)
                .then(a.specificity().cmp(&b.specificity()))
                .then(b.def.name.cmp(&a.def.name))
        });
    match winner {
        None => Outcome::Unmatched,
        Some((i, rule)) => match rule.extract(&text, input.sent_at) {
            Ok(fields) => Outcome::Matched { rule: i, fields },
            Err((error, fields)) => Outcome::Failed {
                rule: i,
                error,
                fields,
            },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::schema::NEW_RULE;

    fn now() -> Timestamp {
        "2026-10-18T12:00:00Z".parse().unwrap()
    }

    fn rule(yaml: &str) -> CompiledRule {
        compile("r", 1, RuleDef::from_yaml(yaml).unwrap()).unwrap()
    }

    fn input(text: &str) -> Input<'_> {
        Input {
            channel: Channel::Share,
            sender: None,
            text,
            sent_at: now(),
        }
    }

    #[test]
    fn normalises_whitespace_and_unicode() {
        // "é" as e + combining accent becomes one character.
        assert_eq!(
            normalize("  Olá\u{a0}\u{a0}Ana \r\n\r\n\r\n\tCo\u{301}digo:  1 "),
            "Olá Ana\n\nCódigo: 1"
        );
    }

    #[test]
    fn the_template_reads_a_ctt_sms() {
        let r = rule(NEW_RULE);
        let text = "CTT: A sua encomenda RR123456789PT está disponível para levantamento no Cacifo X. Código: 482913.";
        let out = evaluate(std::slice::from_ref(&r), &input(text));
        let Outcome::Matched { rule: 0, fields } = out else {
            panic!("{out:?}")
        };
        assert_eq!(fields["tracking_number"], "RR123456789PT");
        assert_eq!(fields["code"], "482913");
        assert_eq!(fields["status"], "ready_for_pickup");
        assert_eq!(fields["code_kind"], "pickup");
        assert_eq!(fields["courier"], "CTT");
        // `none` excludes returns, and other channels don't match.
        assert_eq!(
            evaluate(
                std::slice::from_ref(&r),
                &input(&format!("{text} Será devolvida."))
            ),
            Outcome::Unmatched
        );
        let upload = Input {
            channel: Channel::Upload,
            ..input(text)
        };
        assert_eq!(evaluate(&[r], &upload), Outcome::Unmatched);
    }

    #[test]
    fn a_missing_identifier_is_an_error_unless_follow_up() {
        let r = rule(
            "slug: a\nname: A\nmatch: {all: ['levantamento']}\nextract: {code: '(?P<v>\\d{6})'}\n",
        );
        let out = evaluate(&[r], &input("disponível para levantamento, código 482913"));
        let Outcome::Failed { error, fields, .. } = out else {
            panic!("{out:?}")
        };
        assert!(error.contains("no tracking number"), "{error}");
        assert_eq!(fields["code"], "482913");
        let r = rule("slug: a\nname: A\nfollowUp: true\nmatch: {all: ['levantamento']}\n");
        assert!(matches!(
            evaluate(&[r], &input("levantamento")),
            Outcome::Matched { .. }
        ));
    }

    #[test]
    fn priority_then_specificity_decides() {
        let general = rule(
            "slug: g\nname: G\npriority: 10\nmatch: {all: ['CTT']}\nextract: {tracking_number: '(?P<v>RR\\d+PT)'}\n",
        );
        let specific = rule(
            "slug: s\nname: S\npriority: 10\nmatch: {all: ['CTT', 'levantamento']}\nextract: {tracking_number: '(?P<v>RR\\d+PT)'}\n",
        );
        let urgent = rule(
            "slug: u\nname: U\npriority: 50\nmatch: {all: ['RR']}\nextract: {tracking_number: '(?P<v>RR\\d+PT)'}\n",
        );
        let mut rules = vec![general, specific];
        let text = "CTT RR123PT levantamento";
        assert_eq!(evaluate(&rules, &input(text)).rule(), Some(1));
        rules.push(urgent);
        assert_eq!(evaluate(&rules, &input(text)).rule(), Some(2));
    }

    #[test]
    fn senders_are_checked_only_when_known() {
        let r = rule(
            "slug: a\nname: A\nfollowUp: true\nmatch: {sender: ['*@ctt.pt', 'CTT'], all: ['x']}\n",
        );
        assert!(r.matches(Channel::Share, None, "x"));
        assert!(r.matches(Channel::Upload, Some("Info@CTT.pt"), "x"));
        assert!(r.matches(Channel::Upload, Some("ctt"), "x"));
        assert!(!r.matches(Channel::Upload, Some("noreply@shop.pt"), "x"));
        assert!(glob_matches("a*b*c", "aXXbYYc"));
        assert!(!glob_matches("a*b*c", "aXXcYYb"));
    }

    #[test]
    fn bad_rules_say_what_is_wrong() {
        let def = RuleDef::from_yaml(
            "slug: a\nname: A\nmatch: {all: ['(?=x)', '(']}\nextract: {code: '\\d+', eta: '(?P<v>x'}\n",
        )
        .unwrap();
        let errors = compile("r", 1, def).err().unwrap();
        assert_eq!(errors.len(), 4, "{errors:?}");
        assert!(
            errors[0].starts_with("match.all[0]") && errors[0].contains("look-around"),
            "{errors:?}"
        );
        assert!(
            errors
                .iter()
                .any(|e| e.starts_with("extract.code: needs a named group")),
            "{errors:?}"
        );
        let empty = RuleDef::from_yaml("slug: a\nname: A\nmatch: {}\n").unwrap();
        assert!(compile("r", 1, empty).is_err());
        let huge = RuleDef::from_yaml(&format!(
            "slug: a\nname: A\nmatch: {{all: ['{}']}}\n",
            "a".repeat(2000)
        ))
        .unwrap();
        assert!(compile("r", 1, huge).err().unwrap()[0].contains("longer than"));
    }

    #[test]
    fn dates() {
        let d = |s| parse_date(s, now()).map(|d| d.to_string());
        assert_eq!(d("2026-10-21").as_deref(), Some("2026-10-21"));
        assert_eq!(d("21/10/2026").as_deref(), Some("2026-10-21"));
        assert_eq!(d("21-10-26").as_deref(), Some("2026-10-21"));
        assert_eq!(d("21.10").as_deref(), Some("2026-10-21"));
        // Early January, said in October, is next year.
        assert_eq!(d("05/01").as_deref(), Some("2027-01-05"));
        assert_eq!(d("31/02/2026"), None);
        assert_eq!(d("tomorrow"), None);
        let sent = d("hoje").unwrap();
        assert_eq!(d("Hoje ").as_deref(), Some(sent.as_str()));
        assert!(d("amanhã").unwrap() > sent);
        assert!(d("amanha").unwrap() > sent);
        assert!(d("ontem").unwrap() < sent);
    }
}
