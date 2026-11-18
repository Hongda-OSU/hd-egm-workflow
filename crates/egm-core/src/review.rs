//! Decisions, GO verdicts and the handoff (PRD §6.4, §6.6). Pure functions
//! first, then the file-level operations the UI calls.

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::model::{Advice, Decision, Issue, Role, Severity, Triage, Verdict};
use crate::store::{latest_round, read_meta, read_state, write_atomic, write_json, write_state, PrdMeta, PrdStatus, Stage};
use crate::Result;

/// Retell needs at least this share of the key-decision list (PRD §6.6).
pub const HIT_THRESHOLD: f64 = 0.9;

/// Recomputed conclusions of a round, stored in round.json.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoundVerdicts {
    pub retell: Verdict,
    pub critic: Verdict,
    pub ready: Verdict,
    pub prd: Verdict,
}

/// An issue no longer counts toward GO when the author rejected it, or when
/// it matched an earlier rejection and the author has not overridden that.
/// A manual decision always wins over `rejected_as`.
pub fn is_rejected(issue: &Issue, decision: Option<Advice>) -> bool {
    match decision {
        Some(d) => d == Advice::Reject,
        None => issue.rejected_as.is_some(),
    }
}

fn counts_for(issue: &Issue, role: Role) -> bool {
    issue.source == role || issue.also_from.contains(&role)
}

/// PRD §6.6: a line is GO when none of the issues counted toward it is an
/// unrejected high; retell also needs the hit rate. The PRD is GO when all
/// three lines are GO and the round has no block-conversion warnings. The
/// ready goldfish's own GO / NO-GO does not count.
pub fn compute_verdicts(triage: &Triage, decisions: &[Decision], warnings: &[String]) -> RoundVerdicts {
    let by_id: HashMap<&str, Advice> = decisions.iter().map(|d| (d.issue_id.as_str(), d.decision)).collect();
    let blocked = |role: Role| {
        triage.issues.iter().any(|i| {
            counts_for(i, role)
                && i.severity == Severity::High
                && !is_rejected(i, by_id.get(i.id.as_str()).copied())
        })
    };
    let v = |go: bool| if go { Verdict::Go } else { Verdict::NoGo };
    let retell = v(!blocked(Role::Retell) && triage.hit_rate() >= HIT_THRESHOLD);
    let critic = v(!blocked(Role::Critic));
    let ready = v(!blocked(Role::Ready));
    let all = [retell, critic, ready].iter().all(|x| *x == Verdict::Go) && warnings.is_empty();
    RoundVerdicts { retell, critic, ready, prd: v(all) }
}

/// Issues the author still has to decide: everything except those that
/// matched an earlier rejection and were left alone.
pub fn undecided<'a>(triage: &'a Triage, decisions: &[Decision]) -> Vec<&'a Issue> {
    triage
        .issues
        .iter()
        .filter(|i| i.rejected_as.is_none() && !decisions.iter().any(|d| d.issue_id == i.id))
        .collect()
}

pub fn read_triage(round_dir: &Path) -> Result<Triage> {
    Ok(serde_json::from_str(&fs::read_to_string(round_dir.join("triage.json"))?)?)
}

pub fn read_decisions(round_dir: &Path) -> Result<Vec<Decision>> {
    match fs::read_to_string(round_dir.join("decisions.json")) {
        Ok(text) => Ok(serde_json::from_str(&text)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e.into()),
    }
}

/// Recomputes and stores the round's verdicts; when it is the PRD's latest
/// round, also updates prd.json `status` (history never changes it).
pub fn refresh_verdicts(round_dir: &Path) -> Result<RoundVerdicts> {
    let triage = read_triage(round_dir)?;
    let decisions = read_decisions(round_dir)?;
    let mut state = read_state(round_dir)?;
    let verdicts = compute_verdicts(&triage, &decisions, &state.warnings);
    state.verdicts = Some(verdicts);
    write_state(round_dir, &state)?;

    let prd_dir = round_dir.parent().ok_or("round dir has no parent")?;
    if latest_round(prd_dir).as_deref() == Some(round_dir)
        && let Some(mut meta) = read_meta(prd_dir)
    {
        meta.status = if verdicts.prd == Verdict::Go { PrdStatus::Go } else { PrdStatus::InReview };
        write_meta(prd_dir, &meta)?;
    }
    Ok(verdicts)
}

fn write_meta(prd_dir: &Path, meta: &PrdMeta) -> Result<()> {
    write_json(&prd_dir.join("prd.json"), meta)
}

fn ensure_latest(round_dir: &Path) -> Result<()> {
    let prd_dir = round_dir.parent().ok_or("round dir has no parent")?;
    if latest_round(prd_dir).as_deref() != Some(round_dir) {
        return Err("only the latest round can be decided; earlier rounds are read-only".into());
    }
    Ok(())
}

/// Records (or changes) the author's decision on one issue, then recomputes
/// the verdicts. Only the latest round accepts decisions (PRD §6.4).
pub fn decide(round_dir: &Path, issue_id: &str, decision: Advice, note: Option<String>) -> Result<RoundVerdicts> {
    ensure_latest(round_dir)?;
    let triage = read_triage(round_dir)?;
    if !triage.issues.iter().any(|i| i.id == issue_id) {
        return Err(format!("no issue {issue_id} in this round").into());
    }
    let mut decisions = read_decisions(round_dir)?;
    let entry = Decision {
        issue_id: issue_id.to_string(),
        decision,
        note: note.filter(|n| !n.trim().is_empty()),
        decided_at: now_rfc3339(),
    };
    match decisions.iter_mut().find(|d| d.issue_id == issue_id) {
        Some(d) => *d = entry,
        None => decisions.push(entry),
    }
    write_json(&round_dir.join("decisions.json"), &decisions)?;
    refresh_verdicts(round_dir)
}

fn cell(s: &str) -> String {
    s.replace('|', "\\|").replace('\n', "<br>")
}

fn severity_rank(s: Severity) -> u8 {
    match s {
        Severity::High => 0,
        Severity::Medium => 1,
        Severity::Low => 2,
    }
}

fn severity_name(s: Severity) -> &'static str {
    match s {
        Severity::High => "high",
        Severity::Medium => "medium",
        Severity::Low => "low",
    }
}

/// handoff.md (PRD §6.4 template). Rejection reasons come only from the
/// author: their note, else the earlier rejection it matched, else
/// "（未填）" — never the triager's reasoning.
pub fn render_handoff(title: &str, triage: &Triage, decisions: &[Decision]) -> String {
    let by_id: HashMap<&str, &Decision> = decisions.iter().map(|d| (d.issue_id.as_str(), d)).collect();
    let decision_of = |i: &Issue| by_id.get(i.id.as_str()).map(|d| d.decision);
    let note_of = |i: &Issue| by_id.get(i.id.as_str()).and_then(|d| d.note.clone()).unwrap_or_default();

    let mut accepted: Vec<&Issue> =
        triage.issues.iter().filter(|i| decision_of(i) == Some(Advice::Accept)).collect();
    accepted.sort_by_key(|i| severity_rank(i.severity));
    let rejected: Vec<&Issue> =
        triage.issues.iter().filter(|i| is_rejected(i, decision_of(i))).collect();

    let mut md = format!("# 交接：{title} · 第 {} 轮\n\n", triage.round);
    md.push_str("## 已接受（按严重度排列）\n\n");
    if accepted.is_empty() {
        md.push_str("（无）\n");
    } else {
        md.push_str("| # | 严重度 | 章节 | 问题 | 建议改法 | 备注 |\n| --- | --- | --- | --- | --- | --- |\n");
        for i in accepted {
            md.push_str(&format!(
                "| {} | {} | {} | {}：{} | {} | {} |\n",
                i.id,
                severity_name(i.severity),
                cell(&i.section),
                cell(&i.title),
                cell(&i.problem),
                cell(i.suggested_change.as_deref().unwrap_or("")),
                cell(&note_of(i)),
            ));
        }
    }
    md.push_str("\n## 已驳回（不要加进 PRD）\n\n");
    if rejected.is_empty() {
        md.push_str("（无）\n");
    } else {
        md.push_str("| # | 问题 | 驳回理由 |\n| --- | --- | --- |\n");
        for i in rejected {
            let note = note_of(i);
            let reason = match (&i.rejected_as, decision_of(i)) {
                _ if !note.is_empty() => note,
                (Some(prev), None) => format!("与历史驳回 {prev} 相同"),
                _ => "（未填）".to_string(),
            };
            md.push_str(&format!("| {} | {} | {} |\n", i.id, cell(&i.title), cell(&reason)));
        }
    }
    md
}

/// Writes handoff.md once every issue that needs a decision has one, and
/// marks the round done (PRD §6.4, §7.2). Regenerating overwrites it.
pub fn write_handoff(round_dir: &Path) -> Result<String> {
    ensure_latest(round_dir)?;
    let triage = read_triage(round_dir)?;
    let decisions = read_decisions(round_dir)?;
    let open = undecided(&triage, &decisions);
    if !open.is_empty() {
        let ids: Vec<&str> = open.iter().map(|i| i.id.as_str()).collect();
        return Err(format!("decide these first: {}", ids.join(", ")).into());
    }
    let md = render_handoff(&prd_title(round_dir), &triage, &decisions);
    write_atomic(&round_dir.join("handoff.md"), md.as_bytes())?;
    let mut state = read_state(round_dir)?;
    state.stage = Stage::Done;
    write_state(round_dir, &state)?;
    Ok(md)
}

fn prd_title(round_dir: &Path) -> String {
    round_dir.parent().and_then(read_meta).map(|m| m.title).unwrap_or_default()
}

/// True when handoff.md exists but no longer matches what the current
/// decisions would produce, i.e. a decision or note changed after it was
/// written. Compared by content, so changing a decision and changing it
/// back is not stale. Anything unreadable counts as not stale.
pub fn handoff_stale(round_dir: &Path) -> bool {
    let Ok(saved) = fs::read_to_string(round_dir.join("handoff.md")) else { return false };
    let (Ok(triage), Ok(decisions)) = (read_triage(round_dir), read_decisions(round_dir)) else { return false };
    render_handoff(&prd_title(round_dir), &triage, &decisions) != saved
}

/// UTC timestamp, e.g. 2026-11-15T20:30:00Z.
pub fn now_rfc3339() -> String {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()) as i64;
    let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
    let t = secs.rem_euclid(86_400);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", t / 3600, t % 3600 / 60, t % 60)
}

/// Days since 1970-01-01 -> (year, month, day), proleptic Gregorian.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{dec, issue, triage};

    #[test]
    fn no_issues_and_enough_hits_is_go() {
        let v = compute_verdicts(&triage(vec![], 9, 10), &[], &[]);
        assert_eq!(v.prd, Verdict::Go);
    }

    #[test]
    fn low_hit_rate_blocks_retell_only() {
        let v = compute_verdicts(&triage(vec![], 8, 10), &[], &[]);
        assert_eq!((v.retell, v.critic, v.ready, v.prd), (Verdict::NoGo, Verdict::Go, Verdict::Go, Verdict::NoGo));
    }

    #[test]
    fn merged_high_blocks_every_line_it_counts_for() {
        let t = triage(vec![issue("r2-01", Role::Critic, &[Role::Ready], Severity::High, None)], 10, 10);
        let v = compute_verdicts(&t, &[], &[]);
        assert_eq!((v.retell, v.critic, v.ready), (Verdict::Go, Verdict::NoGo, Verdict::NoGo));
    }

    #[test]
    fn rejecting_the_last_high_makes_it_go() {
        let t = triage(vec![issue("r2-01", Role::Critic, &[], Severity::High, None)], 10, 10);
        let v = compute_verdicts(&t, &[dec("r2-01", Advice::Reject)], &[]);
        assert_eq!(v.prd, Verdict::Go);
    }

    #[test]
    fn manual_accept_overrides_rejected_as() {
        let t = triage(vec![issue("r3-01", Role::Critic, &[], Severity::High, Some("r2-09"))], 10, 10);
        assert_eq!(compute_verdicts(&t, &[], &[]).critic, Verdict::Go);
        assert_eq!(compute_verdicts(&t, &[dec("r3-01", Advice::Accept)], &[]).critic, Verdict::NoGo);
    }

    #[test]
    fn medium_issues_and_warnings() {
        let t = triage(vec![issue("r2-01", Role::Ready, &[], Severity::Medium, None)], 10, 10);
        assert_eq!(compute_verdicts(&t, &[], &[]).prd, Verdict::Go);
        assert_eq!(compute_verdicts(&t, &[], &["unsupported block".into()]).prd, Verdict::NoGo);
    }

    #[test]
    fn auto_rejected_issues_need_no_decision() {
        let t = triage(
            vec![
                issue("r3-01", Role::Critic, &[], Severity::High, Some("r2-09")),
                issue("r3-02", Role::Critic, &[], Severity::Low, None),
            ],
            10,
            10,
        );
        let open: Vec<&str> = undecided(&t, &[]).iter().map(|i| i.id.as_str()).collect();
        assert_eq!(open, vec!["r3-02"]);
    }

    #[test]
    fn handoff_uses_author_reasons_only() {
        let t = triage(
            vec![
                issue("r3-01", Role::Critic, &[], Severity::Low, None),
                issue("r3-02", Role::Critic, &[], Severity::High, None),
                issue("r3-03", Role::Ready, &[], Severity::Medium, Some("r2-09")),
                issue("r3-04", Role::Ready, &[], Severity::Medium, None),
            ],
            10,
            10,
        );
        let mut with_note = dec("r3-04", Advice::Reject);
        with_note.note = Some("设计如此".into());
        let md = render_handoff(
            "P",
            &t,
            &[dec("r3-01", Advice::Accept), dec("r3-02", Advice::Accept), with_note],
        );
        assert!(md.starts_with("# 交接：P · 第 2 轮"));
        let high = md.find("| r3-02 | high").unwrap();
        let low = md.find("| r3-01 | low").unwrap();
        assert!(high < low, "accepted issues are sorted by severity");
        assert!(md.contains("| r3-03 | t-r3-03 | 与历史驳回 r2-09 相同 |"));
        assert!(md.contains("| r3-04 | t-r3-04 | 设计如此 |"));
        assert!(!md.contains("| r |"), "never uses the triager's reason");
    }

    #[test]
    fn handoff_goes_stale_only_when_its_content_would_change() {
        let tmp = crate::testutil::TempDir::new("stale");
        let rd = tmp.path().join("round-1");
        fs::create_dir_all(&rd).unwrap();
        let t = triage(vec![issue("r1-01", Role::Critic, &[], Severity::High, None)], 10, 10);
        write_json(&rd.join("triage.json"), &t).unwrap();
        assert!(!handoff_stale(&rd), "no handoff yet");

        let accepted = vec![dec("r1-01", Advice::Accept)];
        write_json(&rd.join("decisions.json"), &accepted).unwrap();
        fs::write(rd.join("handoff.md"), render_handoff("", &t, &accepted)).unwrap();
        assert!(!handoff_stale(&rd));

        write_json(&rd.join("decisions.json"), &vec![dec("r1-01", Advice::Reject)]).unwrap();
        assert!(handoff_stale(&rd));

        write_json(&rd.join("decisions.json"), &accepted).unwrap();
        assert!(!handoff_stale(&rd), "changing back matches the saved file again");
    }

    #[test]
    fn timestamps() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_407), (2025, 11, 15));
        assert!(now_rfc3339().ends_with('Z'));
    }
}
