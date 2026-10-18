//! Redaction: what leaves the cluster (to an LLM, or in an exported rule) has
//! its personal data replaced by typed, numbered placeholders that keep the
//! text's shape: `‹NAME_1›`, `‹ADDRESS_1›`, `‹PHONE_1›`…
//!
//! Three layers, strongest first:
//! 1. **Known values** from the household settings (names, addresses,
//!    postcodes, phone numbers, emails), matched ignoring case and accents.
//! 2. **Patterns**: emails, phone numbers, PT postcodes, NIF (check digit
//!    validated), IBAN and card numbers (checksums validated), the path
//!    tokens and query values of URLs (the host stays) and the name in a
//!    greeting.
//! 3. [`find_leak`], run on everything about to be sent: a last check that no
//!    known value is left, whatever was typed into the text since.
//!
//! Not redacted: tracking numbers, delivery and pickup codes, merchant and
//! courier names, dates: they are what a rule has to find. Codes are only
//! replaced when asked to ([`Options::redact_codes`]).

use std::collections::BTreeMap;
use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;
use unicode_normalization::char::is_combining_mark;

/// The people and places of the household, as typed in the settings.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Household {
    #[serde(default)]
    pub names: Vec<String>,
    #[serde(default)]
    pub addresses: Vec<String>,
    #[serde(default)]
    pub postcodes: Vec<String>,
    #[serde(default)]
    pub phones: Vec<String>,
    #[serde(default)]
    pub emails: Vec<String>,
}

impl Household {
    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
            && self.addresses.is_empty()
            && self.postcodes.is_empty()
            && self.phones.is_empty()
            && self.emails.is_empty()
    }

    /// One value per line, as the settings form shows them.
    pub fn parse_lines(text: &str) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            if !line.is_empty() && !out.iter().any(|l| l == line) {
                out.push(line.to_string());
            }
        }
        out
    }

    fn entries(&self) -> Vec<(Kind, &str)> {
        let mut all = Vec::new();
        for (kind, list) in [
            (Kind::Email, &self.emails),
            (Kind::Phone, &self.phones),
            (Kind::Postcode, &self.postcodes),
            (Kind::Address, &self.addresses),
            (Kind::Name, &self.names),
        ] {
            all.extend(list.iter().map(|v| (kind, v.as_str())));
        }
        all
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    Name,
    Address,
    Postcode,
    Phone,
    Email,
    Nif,
    Iban,
    Card,
    Token,
    Code,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Name => "NAME",
            Kind::Address => "ADDRESS",
            Kind::Postcode => "POSTCODE",
            Kind::Phone => "PHONE",
            Kind::Email => "EMAIL",
            Kind::Nif => "NIF",
            Kind::Iban => "IBAN",
            Kind::Card => "CARD",
            Kind::Token => "TOKEN",
            Kind::Code => "CODE",
        }
    }

    /// Which overlapping match wins: known values, then the most specific
    /// patterns.
    fn priority(self, known: bool) -> u8 {
        if known {
            return 100;
        }
        match self {
            Kind::Email => 90,
            Kind::Iban => 85,
            Kind::Card => 80,
            Kind::Token => 70,
            Kind::Code => 65,
            Kind::Phone => 60,
            Kind::Nif => 50,
            Kind::Postcode => 40,
            Kind::Name | Kind::Address => 30,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Options {
    /// Also replace delivery and pickup codes, with a same-length
    /// placeholder of the same character class (`482913` → `‹CODE:dddddd›`).
    pub redact_codes: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Redacted {
    pub text: String,
    /// Distinct placeholders used, by kind (for showing what was replaced).
    pub counts: BTreeMap<&'static str, usize>,
}

struct Span {
    start: usize,
    end: usize,
    kind: Kind,
    known: bool,
    /// What makes two matches "the same thing" (so they share a placeholder).
    key: String,
}

/// Text with case, accents and whitespace runs folded away, remembering
/// where each folded character came from.
struct Folded {
    chars: Vec<char>,
    /// Byte range in the original of each folded character.
    spans: Vec<(usize, usize)>,
}

fn fold(text: &str) -> Folded {
    let mut chars: Vec<char> = Vec::new();
    let mut spans: Vec<(usize, usize)> = Vec::new();
    for (i, c) in text.char_indices() {
        let end = i + c.len_utf8();
        if is_combining_mark(c) {
            if let Some(last) = spans.last_mut() {
                last.1 = end;
            }
            continue;
        }
        if c.is_whitespace() {
            if chars.last() == Some(&' ') {
                spans.last_mut().expect("a space was pushed").1 = end;
            } else {
                chars.push(' ');
                spans.push((i, end));
            }
            continue;
        }
        for d in std::iter::once(c).nfd() {
            if is_combining_mark(d) {
                continue;
            }
            for l in d.to_lowercase() {
                chars.push(l);
                spans.push((i, end));
            }
        }
    }
    Folded { chars, spans }
}

fn folded_needle(value: &str) -> Vec<char> {
    let mut chars = fold(value.trim()).chars;
    while chars.first() == Some(&' ') {
        chars.remove(0);
    }
    while chars.last() == Some(&' ') {
        chars.pop();
    }
    chars
}

/// Finds `needle` in `hay` where it is not part of a longer word.
fn find_all(hay: &Folded, needle: &[char], bounded: bool) -> Vec<(usize, usize)> {
    let mut found = Vec::new();
    if needle.is_empty() || needle.len() > hay.chars.len() {
        return found;
    }
    let mut i = 0;
    while i + needle.len() <= hay.chars.len() {
        if hay.chars[i..i + needle.len()] == *needle {
            let before = i.checked_sub(1).map(|j| hay.chars[j]);
            let after = hay.chars.get(i + needle.len()).copied();
            let word = |c: Option<char>| c.is_some_and(char::is_alphanumeric);
            let edge_start = needle[0].is_alphanumeric();
            let edge_end = needle[needle.len() - 1].is_alphanumeric();
            if !bounded || !((edge_start && word(before)) || (edge_end && word(after))) {
                found.push((hay.spans[i].0, hay.spans[i + needle.len() - 1].1));
                i += needle.len();
                continue;
            }
        }
        i += 1;
    }
    found
}

fn digits(value: &str) -> String {
    value.chars().filter(char::is_ascii_digit).collect()
}

/// A regex matching a phone number's digits with any usual separators, with
/// or without the Portuguese prefix.
fn phone_regex(value: &str) -> Option<Regex> {
    let d = digits(value);
    let d = d
        .strip_prefix("00351")
        .or_else(|| d.strip_prefix("351").filter(|r| r.len() == 9))
        .unwrap_or(&d);
    if d.len() < 6 {
        return None;
    }
    let body: Vec<String> = d.chars().map(|c| c.to_string()).collect();
    Regex::new(&format!(
        r"(?:(?:\+|00)?351[ .\-]?)?{}",
        body.join(r"[ .\-]?")
    ))
    .ok()
}

fn alnum_edge(text: &str, start: usize, end: usize) -> bool {
    let before = text[..start].chars().next_back();
    let after = text[end..].chars().next();
    !before.is_some_and(char::is_alphanumeric) && !after.is_some_and(char::is_alphanumeric)
}

static EMAIL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[A-Za-z0-9._%+\-]+@[A-Za-z0-9\-]+(?:\.[A-Za-z0-9\-]+)*\.[A-Za-z]{2,}").unwrap()
});
static PHONE_PT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:(?:\+|00)351[ .\-]?)?[239]\d{2}[ .\-]?\d{3}[ .\-]?\d{3}").unwrap()
});
static PHONE_INTL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\+\d{1,3}[ .\-]?\d(?:[ .\-]?\d){6,12}").unwrap());
static POSTCODE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\d{4}-\d{3}").unwrap());
static NINE_DIGITS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\d{9}").unwrap());
static IBAN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[A-Z]{2}\d{2}(?: ?[A-Z0-9]{4}){2,7}(?: ?[A-Z0-9]{1,3})?").unwrap()
});
static CARD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\d(?:[ \-]?\d){12,18}").unwrap());
static URL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"https?://[^\s<>"'\)\]]+"#).unwrap());
static GREETING: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"\b(?i:ol[áa]|caro\(a\)|caro|cara|exmo\.?\(a\)|exm[oa]\.?|dear|hello|hi|hey)[ \t]+(?P<n>\p{Lu}[\p{L}'\-]+(?:[ \t]+\p{Lu}[\p{L}'\-]+){0,3})",
    )
    .unwrap()
});
static CODE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i:c[óo]digo|code|pin|senha)\D{0,20}?(?P<c>\d[A-Za-z0-9]{3,7})\b").unwrap()
});

/// The NIF check digit: weights 9..2 over the first eight digits.
pub fn valid_nif(d: &str) -> bool {
    let n: Vec<u32> = d.chars().filter_map(|c| c.to_digit(10)).collect();
    if n.len() != 9 || !matches!(n[0], 1 | 2 | 3 | 5 | 6 | 8 | 9) {
        return false;
    }
    let sum: u32 = n[..8]
        .iter()
        .enumerate()
        .map(|(i, d)| d * (9 - i as u32))
        .sum();
    let check = match 11 - sum % 11 {
        10 | 11 => 0,
        c => c,
    };
    check == n[8]
}

pub fn luhn(d: &str) -> bool {
    let n: Vec<u32> = d.chars().filter_map(|c| c.to_digit(10)).collect();
    if n.len() < 13 {
        return false;
    }
    let sum: u32 = n
        .iter()
        .rev()
        .enumerate()
        .map(|(i, d)| {
            if i % 2 == 1 {
                let x = d * 2;
                if x > 9 { x - 9 } else { x }
            } else {
                *d
            }
        })
        .sum();
    sum.is_multiple_of(10)
}

pub fn valid_iban(s: &str) -> bool {
    let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    if !(15..=34).contains(&s.len()) {
        return false;
    }
    let rearranged = format!("{}{}", &s[4..], &s[..4]);
    let mut rem: u32 = 0;
    for c in rearranged.chars() {
        let Some(v) = c.to_digit(36) else {
            return false;
        };
        rem = if v >= 10 {
            (rem * 100 + v) % 97
        } else {
            (rem * 10 + v) % 97
        };
    }
    rem == 1
}

fn push(spans: &mut Vec<Span>, start: usize, end: usize, kind: Kind, known: bool, key: &str) {
    spans.push(Span {
        start,
        end,
        kind,
        known,
        key: key.to_string(),
    });
}

fn known_spans(text: &str, household: &Household, spans: &mut Vec<Span>) {
    let hay = fold(text);
    for (kind, value) in household.entries() {
        let needle = folded_needle(value);
        let key: String = needle.iter().collect();
        if kind == Kind::Phone
            && let Some(re) = phone_regex(value)
        {
            for m in re.find_iter(text) {
                if alnum_edge(text, m.start(), m.end()) {
                    push(spans, m.start(), m.end(), kind, true, &digits(value));
                }
            }
        }
        // Without word boundaries for emails and postcodes, which sit inside
        // punctuation; with them for names and addresses, so that "Ana"
        // doesn't match inside "banana".
        let bounded = !matches!(kind, Kind::Email);
        for (s, e) in find_all(&hay, &needle, bounded) {
            push(spans, s, e, kind, true, &key);
        }
    }
}

fn pattern_spans(text: &str, opts: Options, spans: &mut Vec<Span>) {
    for m in EMAIL.find_iter(text) {
        push(
            spans,
            m.start(),
            m.end(),
            Kind::Email,
            false,
            &m.as_str().to_lowercase(),
        );
    }
    for re in [&*PHONE_PT, &*PHONE_INTL] {
        for m in re.find_iter(text) {
            if alnum_edge(text, m.start(), m.end()) {
                push(
                    spans,
                    m.start(),
                    m.end(),
                    Kind::Phone,
                    false,
                    &digits(m.as_str()),
                );
            }
        }
    }
    for m in POSTCODE.find_iter(text) {
        if alnum_edge(text, m.start(), m.end()) {
            push(spans, m.start(), m.end(), Kind::Postcode, false, m.as_str());
        }
    }
    for m in NINE_DIGITS.find_iter(text) {
        if alnum_edge(text, m.start(), m.end()) && valid_nif(m.as_str()) {
            push(spans, m.start(), m.end(), Kind::Nif, false, m.as_str());
        }
    }
    for m in IBAN.find_iter(text) {
        if alnum_edge(text, m.start(), m.end()) && valid_iban(m.as_str()) {
            push(
                spans,
                m.start(),
                m.end(),
                Kind::Iban,
                false,
                &m.as_str().replace(' ', ""),
            );
        }
    }
    for m in CARD.find_iter(text) {
        if alnum_edge(text, m.start(), m.end()) && luhn(&digits(m.as_str())) {
            push(
                spans,
                m.start(),
                m.end(),
                Kind::Card,
                false,
                &digits(m.as_str()),
            );
        }
    }
    for m in URL.find_iter(text) {
        url_spans(
            m.start(),
            m.as_str().trim_end_matches(['.', ',', ';', ':', '!']),
            spans,
        );
    }
    for c in GREETING.captures_iter(text) {
        let n = c.name("n").expect("group n");
        push(
            spans,
            n.start(),
            n.end(),
            Kind::Name,
            false,
            &n.as_str().to_lowercase(),
        );
    }
    if opts.redact_codes {
        for c in CODE.captures_iter(text) {
            let code = c.name("c").expect("group c");
            if code.as_str().chars().any(|ch| ch.is_ascii_digit()) {
                push(
                    spans,
                    code.start(),
                    code.end(),
                    Kind::Code,
                    false,
                    code.as_str(),
                );
            }
        }
    }
}

/// Spans for what in a URL identifies something: long path segments and
/// query values. The scheme, host, short path words and parameter names stay.
fn url_spans(base: usize, url: &str, spans: &mut Vec<Span>) {
    let Some(host_start) = url.find("://").map(|i| i + 3) else {
        return;
    };
    let path_start = url[host_start..]
        .find(['/', '?', '#'])
        .map_or(url.len(), |i| host_start + i);
    let mut at = path_start;
    // The path.
    let path_end = url[at..].find(['?', '#']).map_or(url.len(), |i| at + i);
    for segment in url[at..path_end].split('/') {
        let seg_start = at;
        at += segment.len() + 1;
        let tokenish = segment.len() >= 8 && segment.chars().any(|c| c.is_ascii_digit());
        if tokenish {
            push(
                spans,
                base + seg_start,
                base + seg_start + segment.len(),
                Kind::Token,
                false,
                segment,
            );
        }
    }
    // The query: every parameter's value.
    if let Some(q) = url[path_end..].strip_prefix('?') {
        let q_start = path_end + 1;
        let q = q.split('#').next().unwrap_or(q);
        let mut off = 0;
        for pair in q.split('&') {
            if let Some(eq) = pair.find('=') {
                let value = &pair[eq + 1..];
                if value.len() >= 6 {
                    let s = base + q_start + off + eq + 1;
                    push(spans, s, s + value.len(), Kind::Token, false, value);
                }
            }
            off += pair.len() + 1;
        }
    }
}

/// Keeps the strongest of the overlapping spans (then the longest), in text
/// order.
fn resolve(mut spans: Vec<Span>) -> Vec<Span> {
    spans.sort_by_key(|s| {
        (
            std::cmp::Reverse(s.kind.priority(s.known)),
            std::cmp::Reverse(s.end - s.start),
            s.start,
        )
    });
    let mut accepted: Vec<Span> = Vec::new();
    for s in spans {
        if s.start < s.end && !accepted.iter().any(|a| s.start < a.end && a.start < s.end) {
            accepted.push(s);
        }
    }
    accepted.sort_by_key(|s| s.start);
    accepted
}

/// Replaces the spans with numbered placeholders; the same thing keeps the
/// same number.
fn render(text: &str, accepted: &[Span]) -> Redacted {
    let mut numbers: BTreeMap<(Kind, String), usize> = BTreeMap::new();
    let mut next: BTreeMap<Kind, usize> = BTreeMap::new();
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for s in accepted {
        out.push_str(&text[at..s.start]);
        at = s.end;
        if s.kind == Kind::Code {
            let class: String = text[s.start..s.end]
                .chars()
                .map(|c| match c {
                    c if c.is_ascii_digit() => 'd',
                    c if c.is_ascii_alphabetic() => 'a',
                    _ => c,
                })
                .collect();
            out.push_str(&format!("‹CODE:{class}›"));
            continue;
        }
        let n = *numbers.entry((s.kind, s.key.clone())).or_insert_with(|| {
            let n = next.entry(s.kind).or_default();
            *n += 1;
            *n
        });
        out.push_str(&format!("‹{}_{n}›", s.kind.label()));
    }
    out.push_str(&text[at..]);

    let mut counts: BTreeMap<&'static str, usize> = BTreeMap::new();
    for (kind, _) in numbers.keys() {
        *counts.entry(kind.label()).or_default() += 1;
    }
    Redacted { text: out, counts }
}

/// Redacts `text`.
pub fn redact(text: &str, household: &Household, opts: Options) -> Redacted {
    let mut spans = Vec::new();
    known_spans(text, household, &mut spans);
    pattern_spans(text, opts, &mut spans);
    render(text, &resolve(spans))
}

/// Redaction for a sender address: only the household's own values (a
/// courier's address is what a rule matches on, and isn't personal).
pub fn redact_known(text: &str, household: &Household) -> String {
    let mut spans = Vec::new();
    known_spans(text, household, &mut spans);
    render(text, &resolve(spans)).text
}

/// The kind of the first household value still in `text`, if any. Run on
/// everything about to leave the cluster: redaction can have been edited
/// around, and what leaks must be refused rather than trusted to a regex.
/// Names and addresses count as whole words ("Ana" is not in "banana");
/// emails, phones and postcodes anywhere.
pub fn find_leak(text: &str, household: &Household) -> Option<Kind> {
    let hay = fold(text);
    for (kind, value) in household.entries() {
        let needle = folded_needle(value);
        let bounded = matches!(kind, Kind::Name | Kind::Address);
        if !find_all(&hay, &needle, bounded).is_empty() {
            return Some(kind);
        }
        if kind == Kind::Phone
            && let Some(re) = phone_regex(value)
            && re.is_match(text)
        {
            return Some(kind);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn household() -> Household {
        Household {
            names: vec!["Ana Ferreira".into(), "Ana".into(), "David".into()],
            addresses: vec!["Avenida da Liberdade 10".into()],
            postcodes: vec!["1250-096".into()],
            phones: vec!["+351 912 345 678".into()],
            emails: vec!["ana@example.com".into()],
        }
    }

    fn r(text: &str) -> String {
        redact(text, &household(), Options::default()).text
    }

    #[test]
    fn known_values_ignore_case_accents_and_spacing() {
        assert_eq!(
            r("Para ANA  FERREIRA, avenida   da LIBERDADE 10 — 1250-096 Lisboa"),
            "Para ‹NAME_1›, ‹ADDRESS_1› — ‹POSTCODE_1› Lisboa"
        );
        let accented = Household {
            names: vec!["João Pinto".into()],
            ..Default::default()
        };
        assert_eq!(
            redact("joao pinto e JOÃO PINTO", &accented, Options::default()).text,
            "‹NAME_1› e ‹NAME_1›"
        );
        // Decomposed accents too (o + combining tilde).
        assert_eq!(
            redact("Jo\u{61}\u{303}o Pinto", &accented, Options::default()).text,
            "‹NAME_1›"
        );
    }

    #[test]
    fn names_are_whole_words() {
        assert_eq!(r("banana Ana, Anabela"), "banana ‹NAME_1›, Anabela");
    }

    #[test]
    fn the_same_value_keeps_its_placeholder() {
        assert_eq!(
            r("Ana e David. David e Ana"),
            "‹NAME_1› e ‹NAME_2›. ‹NAME_2› e ‹NAME_1›"
        );
    }

    #[test]
    fn phones_in_any_format() {
        for text in [
            "912345678",
            "912 345 678",
            "+351912345678",
            "00351 912-345-678",
            "+351 912 345 678",
        ] {
            assert_eq!(r(&format!("Ligue {text}.")), "Ligue ‹PHONE_1›.", "{text}");
        }
        // Not one of the household's: still a phone.
        assert_eq!(
            r("Ligue 211 234 567 ou +44 20 7946 0958"),
            "Ligue ‹PHONE_1› ou ‹PHONE_2›"
        );
    }

    #[test]
    fn tracking_numbers_codes_and_dates_stay() {
        let text = "CTT: RR123456789PT, código 482913, entrega 21/10/2026 por Worten, PCK8F3K2Q, 8123004417";
        assert_eq!(r(text), text);
    }

    #[test]
    fn nif_card_and_iban_need_their_checksums() {
        assert_eq!(r("NIF 501964843."), "NIF ‹NIF_1›.");
        assert_eq!(r("NIF 501964844."), "NIF 501964844.");
        assert_eq!(
            r("Cartão 4539 1488 0343 6467 e 4539 1488 0343 6468"),
            "Cartão ‹CARD_1› e 4539 1488 0343 6468"
        );
        assert_eq!(
            r("IBAN PT50 0002 0123 1234 5678 9015 4 ou PT50 0002 0123 1234 5678 9015 5"),
            "IBAN ‹IBAN_1› ou PT50 0002 0123 1234 5678 9015 5"
        );
        assert!(valid_nif("501964843"));
        assert!(luhn("4539148803436467"));
        assert!(valid_iban("GB82 WEST 1234 5698 7654 32"));
    }

    #[test]
    fn emails_and_postcodes() {
        assert_eq!(
            r("Escreva a joana@exemplo.pt ou ana@EXAMPLE.com, Rua A, 4000-123 Porto"),
            "Escreva a ‹EMAIL_1› ou ‹EMAIL_2›, Rua A, ‹POSTCODE_1› Porto"
        );
    }

    #[test]
    fn urls_keep_their_host_and_shape() {
        assert_eq!(
            r("Siga em https://track.paack.app/t/AbC123xYz?lang=pt&token=Zx98LmQw."),
            "Siga em https://track.paack.app/t/‹TOKEN_1›?lang=pt&token=‹TOKEN_2›."
        );
        assert_eq!(
            r("https://www.ctt.pt/feapl_2/app/open/objectSearch/objectSearch.jspx"),
            "https://www.ctt.pt/feapl_2/app/open/objectSearch/objectSearch.jspx"
        );
    }

    #[test]
    fn the_name_in_a_greeting() {
        assert_eq!(
            r("Olá Joana Silva, a sua encomenda"),
            "Olá ‹NAME_1›, a sua encomenda"
        );
        assert_eq!(r("Caro Rui Costa:"), "Caro ‹NAME_1›:");
        assert_eq!(r("Olá, a sua encomenda"), "Olá, a sua encomenda");
    }

    #[test]
    fn codes_only_when_asked() {
        let opts = Options { redact_codes: true };
        let h = Household::default();
        assert_eq!(
            redact("Código de levantamento: 482913. PIN 7a3k", &h, opts).text,
            "Código de levantamento: ‹CODE:dddddd›. PIN ‹CODE:dada›"
        );
        assert_eq!(
            redact("Código: 482913", &h, Options::default()).text,
            "Código: 482913"
        );
    }

    #[test]
    fn counts_say_what_was_replaced() {
        let out = redact("Ana, 912345678 e David", &household(), Options::default());
        assert_eq!(out.counts["NAME"], 2);
        assert_eq!(out.counts["PHONE"], 1);
    }

    #[test]
    fn senders_only_lose_household_values() {
        let h = household();
        assert_eq!(
            redact_known("notificacoes@ctt.pt", &h),
            "notificacoes@ctt.pt"
        );
        assert_eq!(redact_known("ana@example.com", &h), "‹EMAIL_1›");
    }

    #[test]
    fn leaks_are_found_wherever_they_hide() {
        let h = household();
        assert_eq!(find_leak("…ANA@EXAMPLE.COM…", &h), Some(Kind::Email));
        assert_eq!(
            find_leak("liberdade 10", &h),
            None,
            "only the whole address"
        );
        assert_eq!(
            find_leak("rua, avenida da liberdade 10, lisboa", &h),
            Some(Kind::Address)
        );
        assert_eq!(
            find_leak("avenida da liberdade 10b", &h),
            None,
            "another address"
        );
        assert_eq!(find_leak("tel 912.345.678", &h), Some(Kind::Phone));
        assert_eq!(find_leak("‹NAME_1› e ‹ADDRESS_1›", &h), None);
        assert_eq!(find_leak("banana", &h), None);
        assert_eq!(find_leak("Dear Ana!", &h), Some(Kind::Name));
        assert_eq!(find_leak("anything", &Household::default()), None);
    }

    #[test]
    fn what_is_redacted_is_not_a_leak() {
        let h = household();
        let text = "Olá Ana Ferreira, Avenida da Liberdade 10, 1250-096 Lisboa. Tel +351 912 345 678, ana@example.com, David.";
        let out = redact(text, &h, Options::default());
        assert_eq!(find_leak(&out.text, &h), None, "{}", out.text);
    }

    #[test]
    fn parses_settings_lines() {
        assert_eq!(
            Household::parse_lines(" Ana \n\nDavid\r\nAna\n"),
            ["Ana", "David"]
        );
    }
}
