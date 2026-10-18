//! Telling the courier from a tracking number's format, for the manual
//! form. Only formats that belong to a single courier are listed: no courier
//! gets any other special treatment.

use std::sync::LazyLock;

use regex::Regex;

/// (format, courier name as shown).
const FORMATS: &[(&str, &str)] = &[
    // UPU S10 numbers issued in Portugal: two letters, nine digits, PT.
    (r"^[A-Z]{2}[0-9]{9}PT$", "CTT"),
];

static COMPILED: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    FORMATS
        .iter()
        .map(|(re, name)| (Regex::new(re).expect("valid courier format"), *name))
        .collect()
});

/// The courier that issues tracking numbers like `tracking`, if only one does.
pub fn detect(tracking: &str) -> Option<&'static str> {
    let tracking: String = tracking
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_uppercase();
    COMPILED
        .iter()
        .find(|(re, _)| re.is_match(&tracking))
        .map(|(_, name)| *name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_portuguese_s10_numbers() {
        assert_eq!(detect("RR123456789PT"), Some("CTT"));
        assert_eq!(detect(" rr 123 456 789 pt "), Some("CTT"));
        assert_eq!(detect("RR123456789CN"), None);
        assert_eq!(detect("PCK8F3K2Q"), None);
    }
}
