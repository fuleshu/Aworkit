//! Freshness classification for search discovery results.
//!
//! Search snippets are index evidence, not live page content. This module
//! detects queries that need current information, rejects results with clearly
//! stale or contradictory date signals, and records an explicit requirement
//! for the caller to verify retained URLs with `web_extract`.

use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{Duration, NaiveDate, Utc};
use regex::Regex;
use serde::{Deserialize, Serialize};

use super::WebSearchResultV1;

/// Call-level freshness policy requested by the model.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WebSearchFreshnessModeV1 {
    /// Detect live-data intent from the query.
    #[default]
    Auto,
    /// Require current results and live page extraction.
    Current,
    /// Permit historical or undated index results.
    Any,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebSearchFreshnessFindingV1 {
    pub url: String,
    pub status: String,
    pub observed_dates: Vec<String>,
    pub reason: String,
}

/// Non-secret freshness evidence returned with every web-search outcome.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebSearchFreshnessV1 {
    pub status: String,
    pub checked_at_epoch_ms: u64,
    pub maximum_age_days: u64,
    pub evaluated_results: usize,
    pub rejected_results: usize,
    pub extraction_required: bool,
    pub findings: Vec<WebSearchFreshnessFindingV1>,
}

#[derive(Clone, Debug)]
pub(super) struct FreshnessPolicy {
    required: bool,
    today: NaiveDate,
    checked_at_epoch_ms: u64,
    maximum_age_days: u64,
}

#[derive(Default)]
pub(super) struct FreshnessLedger {
    evaluated_results: usize,
    rejected_results: usize,
    findings: Vec<WebSearchFreshnessFindingV1>,
}

pub(super) struct FreshnessEvaluation {
    pub results: Vec<WebSearchResultV1>,
    pub rejected_all: bool,
}

impl FreshnessPolicy {
    pub fn resolve(
        query: &str,
        mode: WebSearchFreshnessModeV1,
        validation_enabled: bool,
        maximum_age_days: u64,
    ) -> Self {
        let required = validation_enabled
            && match mode {
                WebSearchFreshnessModeV1::Auto => query_requires_current_information(query),
                WebSearchFreshnessModeV1::Current => true,
                WebSearchFreshnessModeV1::Any => false,
            };
        Self {
            required,
            today: Utc::now().date_naive(),
            checked_at_epoch_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |duration| duration.as_millis() as u64),
            maximum_age_days,
        }
    }

    #[cfg(test)]
    fn at(required: bool, today: NaiveDate, maximum_age_days: u64) -> Self {
        Self {
            required,
            today,
            checked_at_epoch_ms: 0,
            maximum_age_days,
        }
    }

    pub fn required(&self) -> bool {
        self.required
    }
}

impl FreshnessLedger {
    pub fn evaluate(
        &mut self,
        policy: &FreshnessPolicy,
        results: Vec<WebSearchResultV1>,
    ) -> FreshnessEvaluation {
        if !policy.required {
            return FreshnessEvaluation {
                results,
                rejected_all: false,
            };
        }

        let original_count = results.len();
        let mut retained = Vec::with_capacity(original_count);
        for result in results {
            self.evaluated_results += 1;
            let finding = classify_result(policy, &result);
            if matches!(finding.status.as_str(), "stale" | "conflicting") {
                self.rejected_results += 1;
                if self.findings.len() < 20 {
                    self.findings.push(finding);
                }
            } else {
                retained.push(result);
            }
        }
        FreshnessEvaluation {
            rejected_all: original_count > 0 && retained.is_empty(),
            results: retained,
        }
    }

    pub fn finish(self, policy: &FreshnessPolicy) -> WebSearchFreshnessV1 {
        WebSearchFreshnessV1 {
            status: if policy.required {
                "live_extraction_required".into()
            } else {
                "not_required".into()
            },
            checked_at_epoch_ms: policy.checked_at_epoch_ms,
            maximum_age_days: policy.maximum_age_days,
            evaluated_results: self.evaluated_results,
            rejected_results: self.rejected_results,
            extraction_required: policy.required,
            findings: self.findings,
        }
    }
}

fn classify_result(
    policy: &FreshnessPolicy,
    result: &WebSearchResultV1,
) -> WebSearchFreshnessFindingV1 {
    let mut dates = observed_dates(&format!("{}\n{}", result.title, result.snippet));
    dates.sort_unstable();
    dates.dedup();
    let cutoff = policy.today - Duration::days(policy.maximum_age_days as i64);
    let has_stale = dates.iter().any(|date| *date < cutoff);
    let has_current = dates.iter().any(|date| *date >= cutoff);
    let (status, reason) = match (dates.is_empty(), has_stale, has_current) {
        (true, _, _) => (
            "unknown",
            "No reliable publication date was present in the search metadata; verify the live page.",
        ),
        (false, true, true) => (
            "conflicting",
            "The result contains both current and stale date signals and cannot be trusted as a current snapshot.",
        ),
        (false, true, false) => (
            "stale",
            "Every detected date is older than the configured freshness window.",
        ),
        (false, false, true) => (
            "current",
            "Detected dates fall inside the configured freshness window; live extraction is still required.",
        ),
        (false, false, false) => (
            "unknown",
            "Date signals could not be classified; verify the live page.",
        ),
    };
    WebSearchFreshnessFindingV1 {
        url: result.url.clone(),
        status: status.into(),
        observed_dates: dates.into_iter().map(|date| date.to_string()).collect(),
        reason: reason.into(),
    }
}

fn query_requires_current_information(query: &str) -> bool {
    // Match words rather than raw punctuation so prompts such as `price?`,
    // `today's weather`, and `Preis:` receive the same policy as plain text.
    let searchable = query
        .to_lowercase()
        .chars()
        .map(|character| {
            if character.is_alphanumeric() {
                character
            } else {
                ' '
            }
        })
        .collect::<String>();
    let normalized = format!(
        " {} ",
        searchable.split_whitespace().collect::<Vec<_>>().join(" ")
    );
    let historical = [" history ", " historical ", " archive ", " archived "]
        .iter()
        .any(|marker| normalized.contains(marker));
    let explicit_live = [
        " current ",
        " latest ",
        " today ",
        " tonight ",
        " now ",
        " right now ",
        " live ",
        " breaking ",
        " newest ",
        " cheapest ",
        " aktuell ",
        " heute ",
        " jetzt ",
        " neueste ",
        " guenstigste ",
        " günstigste ",
    ]
    .iter()
    .any(|marker| normalized.contains(marker));
    if historical && !explicit_live {
        return false;
    }
    explicit_live
        || [
            " price ",
            " prices ",
            " pricing ",
            " cost ",
            " weather ",
            " forecast ",
            " score ",
            " scores ",
            " schedule ",
            " availability ",
            " in stock ",
            " exchange rate ",
            " stock price ",
            " preis ",
            " preise ",
            " wetter ",
            " verfügbarkeit ",
        ]
        .iter()
        .any(|marker| normalized.contains(marker))
}

fn observed_dates(text: &str) -> Vec<NaiveDate> {
    let mut dates = Vec::new();
    for captures in iso_date_regex().captures_iter(text) {
        push_date(
            &mut dates,
            captures[1].parse().ok(),
            captures[2].parse().ok(),
            captures[3].parse().ok(),
        );
    }
    for captures in numeric_date_regex().captures_iter(text) {
        push_date(
            &mut dates,
            captures[3].parse().ok(),
            captures[2].parse().ok(),
            captures[1].parse().ok(),
        );
    }
    for captures in month_date_regex().captures_iter(text) {
        let day = captures
            .get(1)
            .and_then(|value| value.as_str().parse().ok())
            .unwrap_or(1);
        let month = captures
            .get(2)
            .and_then(|value| month_number(value.as_str()));
        let year = captures
            .get(3)
            .and_then(|value| value.as_str().parse().ok());
        push_date(&mut dates, year, month, Some(day));
    }
    dates
}

fn push_date(dates: &mut Vec<NaiveDate>, year: Option<i32>, month: Option<u32>, day: Option<u32>) {
    if let (Some(year), Some(month), Some(day)) = (year, month, day)
        && let Some(date) = NaiveDate::from_ymd_opt(year, month, day)
    {
        dates.push(date);
    }
}

fn month_number(value: &str) -> Option<u32> {
    match value.to_lowercase().as_str() {
        "january" | "jan" | "januar" => Some(1),
        "february" | "feb" | "februar" => Some(2),
        "march" | "mar" | "märz" | "maerz" => Some(3),
        "april" | "apr" => Some(4),
        "may" | "mai" => Some(5),
        "june" | "jun" | "juni" => Some(6),
        "july" | "jul" | "juli" => Some(7),
        "august" | "aug" => Some(8),
        "september" | "sep" | "sept" => Some(9),
        "october" | "oct" | "oktober" | "okt" => Some(10),
        "november" | "nov" => Some(11),
        "december" | "dec" | "dezember" | "dez" => Some(12),
        _ => None,
    }
}

fn iso_date_regex() -> &'static Regex {
    static VALUE: OnceLock<Regex> = OnceLock::new();
    VALUE
        .get_or_init(|| Regex::new(r"\b(20\d{2})-(0[1-9]|1[0-2])-(0[1-9]|[12]\d|3[01])\b").unwrap())
}

fn numeric_date_regex() -> &'static Regex {
    static VALUE: OnceLock<Regex> = OnceLock::new();
    VALUE.get_or_init(|| {
        Regex::new(r"\b(0?[1-9]|[12]\d|3[01])[./](0?[1-9]|1[0-2])[./](20\d{2})\b").unwrap()
    })
}

fn month_date_regex() -> &'static Regex {
    static VALUE: OnceLock<Regex> = OnceLock::new();
    VALUE.get_or_init(|| {
        Regex::new(
            r"(?i)\b(?:([0-3]?\d)\s+)?(january|jan|januar|february|feb|februar|march|mar|märz|maerz|april|apr|may|mai|june|jun|juni|july|jul|juli|august|aug|september|sep|sept|october|oct|oktober|okt|november|nov|december|dec|dezember|dez)\s+(20\d{2})\b",
        )
        .unwrap()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(title: &str, snippet: &str) -> WebSearchResultV1 {
        WebSearchResultV1 {
            title: title.into(),
            url: "https://example.com/result".into(),
            snippet: snippet.into(),
        }
    }

    #[test]
    fn current_price_query_rejects_conflicting_index_snapshots() {
        let policy = FreshnessPolicy::at(true, NaiveDate::from_ymd_opt(2026, 9, 3).unwrap(), 45);
        let mut ledger = FreshnessLedger::default();
        let evaluation = ledger.evaluate(
            &policy,
            vec![result(
                "September 2026 prices",
                "Mai 2026 Preise: the indexed price snapshot",
            )],
        );

        assert!(evaluation.rejected_all);
        assert!(evaluation.results.is_empty());
        let summary = ledger.finish(&policy);
        assert_eq!(summary.rejected_results, 1);
        assert_eq!(summary.findings[0].status, "conflicting");
        assert!(summary.extraction_required);
    }

    #[test]
    fn undated_results_are_retained_but_require_live_extraction() {
        let policy = FreshnessPolicy::at(true, NaiveDate::from_ymd_opt(2026, 9, 3).unwrap(), 45);
        let mut ledger = FreshnessLedger::default();
        let evaluation = ledger.evaluate(&policy, vec![result("Current price", "Buy now")]);
        assert_eq!(evaluation.results.len(), 1);
        assert!(!evaluation.rejected_all);
        assert!(ledger.finish(&policy).extraction_required);
    }

    #[test]
    fn historical_queries_do_not_trigger_automatic_freshness() {
        assert!(!query_requires_current_information(
            "historical price archive for 1995"
        ));
        assert!(query_requires_current_information(
            "cheapest current price right now"
        ));
        assert!(query_requires_current_information("Price?"));
    }
}
