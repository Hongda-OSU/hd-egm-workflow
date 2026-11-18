//! Builders and a temp dir shared by unit tests.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::model::{Advice, Decision, Hit, Issue, KeyDecision, Role, Severity, Triage, TriageOutput, Verdict, Verdicts};

pub fn issue(id: &str, source: Role, also: &[Role], sev: Severity, rejected_as: Option<&str>) -> Issue {
    Issue {
        id: id.into(),
        source,
        also_from: also.to_vec(),
        refs: vec![],
        rejected_as: rejected_as.map(Into::into),
        severity: sev,
        section: "§1".into(),
        title: format!("t-{id}"),
        problem: "p".into(),
        impact: "i".into(),
        advice: Advice::Accept,
        reason: "r".into(),
        suggested_change: Some("c".into()),
    }
}

/// A round-2 triage with `hits` of `total` key decisions hit.
pub fn triage(issues: Vec<Issue>, hits: usize, total: usize) -> Triage {
    triage_in(2, issues, hits, total)
}

pub fn triage_in(round: u32, issues: Vec<Issue>, hits: usize, total: usize) -> Triage {
    let key_decisions = (0..total)
        .map(|k| KeyDecision { item: format!("k{k}"), result: if k < hits { Hit::Hit } else { Hit::Miss } })
        .collect();
    let out = TriageOutput {
        verdicts: Verdicts { retell: Verdict::NoGo, critic: Verdict::NoGo, ready: Verdict::NoGo },
        key_decisions,
        spec_checks: vec![],
        issues,
        discarded: vec![],
    };
    Triage::from_output(round, out)
}

pub fn dec(id: &str, d: Advice) -> Decision {
    Decision { issue_id: id.into(), decision: d, note: None, decided_at: "x".into() }
}

/// A fresh directory under the system temp dir, removed on drop.
pub struct TempDir(PathBuf);

impl TempDir {
    pub fn new(label: &str) -> Self {
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let p = std::env::temp_dir().join(format!("egm-test-{label}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&p).unwrap();
        TempDir(p)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
