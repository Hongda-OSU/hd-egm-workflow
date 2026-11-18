//! Typed shapes of goldfish and triager output (PRD §7.3, §7.7). Parsing into
//! these with `deny_unknown_fields` is the schema check on our side.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    High,
    Medium,
    Low,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Verdict {
    #[serde(rename = "GO")]
    Go,
    #[serde(rename = "NO-GO")]
    NoGo,
}

/// The three goldfish whose findings become issues.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Retell,
    Critic,
    Ready,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Retell {
    pub summary: String,
    pub key_decisions: Vec<String>,
    pub guesses: Vec<Guess>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Guess {
    pub section: String,
    pub guess: String,
    pub severity: Severity,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Critic {
    pub issues: Vec<CriticIssue>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CriticIssue {
    pub section: String,
    pub problem: String,
    pub impact: String,
    pub severity: Severity,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ready {
    pub verdict: Verdict,
    pub reason: String,
    pub missing: Vec<Missing>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MissingKind {
    Path,
    Data,
    Interface,
    Error,
    Acceptance,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Missing {
    pub kind: MissingKind,
    pub section: String,
    pub detail: String,
    pub severity: Severity,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Spec {
    pub checks: Vec<Check>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Check {
    pub item: String,
    pub pass: bool,
    pub note: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Hit {
    Hit,
    Partial,
    Miss,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Advice {
    Accept,
    Reject,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Verdicts {
    pub retell: Verdict,
    pub critic: Verdict,
    pub ready: Verdict,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeyDecision {
    pub item: String,
    pub result: Hit,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpecCheck {
    pub item: String,
    pub pass: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Issue {
    pub id: String,
    pub source: Role,
    pub also_from: Vec<Role>,
    pub refs: Vec<String>,
    pub rejected_as: Option<String>,
    pub severity: Severity,
    pub section: String,
    pub title: String,
    pub problem: String,
    pub impact: String,
    pub advice: Advice,
    pub reason: String,
    pub suggested_change: Option<String>,
}

/// A goldfish entry the triager left out of every issue, with why
/// (PRD §6.3: the triager may not drop entries silently).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Discard {
    #[serde(rename = "ref")]
    pub reference: String,
    pub reason: String,
}

/// What the triager returns (`schemas/triage.schema.json`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TriageOutput {
    pub verdicts: Verdicts,
    pub key_decisions: Vec<KeyDecision>,
    pub spec_checks: Vec<SpecCheck>,
    pub issues: Vec<Issue>,
    pub discarded: Vec<Discard>,
}

/// `round-N/triage.json` (PRD §7.3): triager output plus app-computed fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Triage {
    pub round: u32,
    pub verdicts: Verdicts,
    pub key_decisions: Vec<KeyDecision>,
    pub key_decision_hits: f64,
    pub key_decision_total: u32,
    pub spec_checks: Vec<SpecCheck>,
    pub issues: Vec<Issue>,
    /// Absent in rounds triaged before the field existed.
    #[serde(default)]
    pub discarded: Vec<Discard>,
}

impl Triage {
    pub fn from_output(round: u32, out: TriageOutput) -> Triage {
        let hits = out
            .key_decisions
            .iter()
            .map(|k| match k.result {
                Hit::Hit => 1.0,
                Hit::Partial => 0.5,
                Hit::Miss => 0.0,
            })
            .sum();
        Triage {
            round,
            key_decision_total: out.key_decisions.len() as u32,
            key_decision_hits: hits,
            verdicts: out.verdicts,
            key_decisions: out.key_decisions,
            spec_checks: out.spec_checks,
            issues: out.issues,
            discarded: out.discarded,
        }
    }

    pub fn hit_rate(&self) -> f64 {
        if self.key_decision_total == 0 {
            0.0
        } else {
            self.key_decision_hits / f64::from(self.key_decision_total)
        }
    }
}

/// `round-N/decisions.json` entry (PRD §7.4).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Decision {
    pub issue_id: String,
    pub decision: Advice,
    #[serde(default)]
    pub note: Option<String>,
    pub decided_at: String,
}

/// One earlier rejection handed to the triager (`round-N/rejected-history.json`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rejection {
    pub id: String,
    pub section: String,
    pub title: String,
    pub problem: String,
    pub note: Option<String>,
}
