//! Fish and triage stages of a round (PRD §6.3): 4 goldfish in parallel,
//! then the triager once all 4 succeeded. Progress lives in round.json, so
//! `resume` only reruns what is not done yet.

use std::fs;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::task::JoinSet;

use crate::agents::{self, ClaudeConfig, CodexConfig};
use crate::model::{Advice, Critic, Decision, Ready, Rejection, Retell, Spec, Triage, TriageOutput, Verdict};
use crate::settings::Settings;
use crate::store::{read_state, write_json, write_state, Stage, StepStatus};
use crate::Result;

const FISH: [(&str, &str, &str); 4] = [
    ("retell", include_str!("../../../prompts/retell.md"), include_str!("../../../schemas/retell.schema.json")),
    ("critic", include_str!("../../../prompts/critic.md"), include_str!("../../../schemas/critic.schema.json")),
    ("ready", include_str!("../../../prompts/ready.md"), include_str!("../../../schemas/ready.schema.json")),
    ("spec", include_str!("../../../prompts/spec.md"), include_str!("../../../schemas/spec.schema.json")),
];
const TRIAGE_PROMPT: &str = include_str!("../../../prompts/triage.md");
const TRIAGE_SCHEMA: &str = include_str!("../../../schemas/triage.schema.json");
const TRIAGE_SYSTEM: &str = "You are the triager in a cross-model PRD review workflow. \
Follow the triage rules in the user's message and return only the structured result.";

pub struct RoundResult {
    pub dir: PathBuf,
    pub triage: Triage,
    /// App-computed (PRD §6.6), not the triager's own pre-decision guess.
    pub verdicts: crate::review::RoundVerdicts,
    pub warnings: Vec<String>,
}

impl RoundResult {
    pub fn overall_go(&self) -> bool {
        self.verdicts.prd == Verdict::Go
    }
}

fn read(path: &Path) -> Result<String> {
    fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()).into())
}

/// The 交付前检查清单 items of hd-prd-rules, minus those a goldfish cannot
/// judge: marking the title [已完成] (not done while under review) and the
/// 关键决策清单 item (stripped from what goldfish see; `pull` already
/// refuses a PRD without it).
pub fn spec_checklist(prd_rules: &str) -> Result<String> {
    let mut in_section = false;
    let mut items = Vec::new();
    for line in prd_rules.lines() {
        if line.starts_with("## ") {
            in_section = line.contains("交付前检查清单");
            continue;
        }
        let t = line.trim_start();
        let item = t.strip_prefix("- [ ] ").or_else(|| t.strip_prefix("- [x] "));
        if let (true, Some(item)) = (in_section, item)
            && !item.contains("[已完成]")
            && !item.contains(crate::prd::KEY_DECISIONS_HEADING)
        {
            items.push(format!("- {item}"));
        }
    }
    if items.is_empty() {
        return Err("hd-prd-rules has no 交付前检查清单 items".into());
    }
    Ok(items.join("\n"))
}

fn parse<T: DeserializeOwned>(v: &Value) -> Result<T> {
    Ok(serde_json::from_value(v.clone())?)
}

fn validate_fish(name: &str, text: &str) -> Result<Value> {
    let v: Value = serde_json::from_str(text)?;
    match name {
        "retell" => drop(parse::<Retell>(&v)?),
        "critic" => drop(parse::<Critic>(&v)?),
        "ready" => drop(parse::<Ready>(&v)?),
        _ => {
            let spec: Spec = parse(&v)?;
            if spec.checks.iter().any(|c| !c.pass && c.note.trim().is_empty()) {
                return Err("spec: a failed check has no note".into());
            }
        }
    }
    Ok(v)
}

/// One goldfish. Invalid output is retried once (PRD §9); a process failure
/// (quota, login, timeout) is not, so it does not burn quota twice.
async fn run_fish(name: &'static str, prompt: String, schema: &'static str, cfg: CodexConfig) -> (&'static str, u32, Result<Value>) {
    let mut retries = 0;
    loop {
        let text = match agents::codex(&cfg, prompt.clone(), schema).await {
            Ok(text) => text,
            Err(e) => return (name, retries, Err(e)),
        };
        match validate_fish(name, &text) {
            Ok(v) => return (name, retries, Ok(v)),
            Err(_) if retries == 0 => retries += 1,
            Err(e) => return (name, retries, Err(format!("invalid output after a retry: {e}").into())),
        }
    }
}

/// Rejected issues of all earlier rounds (PRD §6.6), from each round's
/// triage.json + decisions.json. A rejection drops out again when a later
/// round matched an issue to it (`rejected_as`) and the author accepted that
/// issue: the author changed their mind.
pub fn collect_rejections(prd_dir: &Path, current: u32) -> Result<Vec<Rejection>> {
    let mut out = Vec::new();
    let mut overturned = Vec::new();
    for k in 1..current {
        let dir = prd_dir.join(format!("round-{k}"));
        let (Ok(t), Ok(d)) = (fs::read_to_string(dir.join("triage.json")), fs::read_to_string(dir.join("decisions.json"))) else {
            continue;
        };
        let triage: Triage = serde_json::from_str(&t)?;
        let decisions: Vec<Decision> = serde_json::from_str(&d)?;
        for dec in &decisions {
            let Some(i) = triage.issues.iter().find(|i| i.id == dec.issue_id) else { continue };
            match (dec.decision, &i.rejected_as) {
                (Advice::Reject, _) => out.push(Rejection {
                    id: i.id.clone(),
                    section: i.section.clone(),
                    title: i.title.clone(),
                    problem: i.problem.clone(),
                    note: dec.note.clone(),
                }),
                (Advice::Accept, Some(prev)) => overturned.push(prev.clone()),
                (Advice::Accept, None) => {}
            }
        }
    }
    out.retain(|r| !overturned.contains(&r.id));
    Ok(out)
}

fn validate_triage(v: &Value, round: u32, key_items: usize) -> Result<TriageOutput> {
    let out: TriageOutput = parse(v)?;
    let prefix = format!("r{round}-");
    let mut ids: Vec<&str> = out.issues.iter().map(|i| i.id.as_str()).collect();
    if ids.iter().any(|id| !id.starts_with(&prefix)) {
        return Err(format!("issue ids must start with {prefix}").into());
    }
    ids.sort_unstable();
    ids.dedup();
    if ids.len() != out.issues.len() {
        return Err("duplicate issue ids".into());
    }
    if out.key_decisions.len() != key_items {
        return Err(format!("scored {} key decisions, list has {key_items}", out.key_decisions.len()).into());
    }
    Ok(out)
}

fn round_number(round_dir: &Path) -> Result<u32> {
    round_dir
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.strip_prefix("round-"))
        .and_then(|n| n.parse().ok())
        .ok_or_else(|| format!("not a round dir: {}", round_dir.display()).into())
}

/// Runs whatever is not done yet in `round_dir`. `log` gets progress lines.
pub async fn run(settings: &Settings, round_dir: &Path, mut log: impl FnMut(String)) -> Result<RoundResult> {
    let round = round_number(round_dir)?;
    let prd_dir = round_dir.parent().ok_or("round dir has no parent")?;
    let rules = round_dir.join("rules");
    let prd = read(&round_dir.join("prd.md"))?;
    let key_decisions = read(&round_dir.join("key-decisions.md"))?;
    let goldfish_rules = read(&rules.join("hd-goldfish-rules.md"))?;
    let elephant_rules = read(&rules.join("hd-elephant-rules.md"))?;
    let checklist = spec_checklist(&read(&rules.join("hd-prd-rules.md"))?)?;
    let mut state = read_state(round_dir)?;
    let fish_dir = round_dir.join("fish");
    fs::create_dir_all(&fish_dir)?;

    // Fish stage.
    let cfg = CodexConfig {
        home: settings.codex_home(),
        model: settings.goldfish_model.clone(),
        effort: settings.goldfish_effort.clone(),
    };
    let mut set = JoinSet::new();
    for (name, template, schema) in FISH {
        if state.fish.get(name).is_some_and(|s| s.status == StepStatus::Done) {
            continue;
        }
        let prompt = template
            .replace("{{goldfish_rules}}", goldfish_rules.trim())
            .replace("{{checklist}}", &checklist)
            .replace("{{prd}}", prd.trim());
        if let Some(step) = state.fish.get_mut(name) {
            step.status = StepStatus::Running;
            step.error = None;
        }
        set.spawn(run_fish(name, prompt, schema, cfg.clone()));
        log(format!("{name}: running"));
    }
    if !set.is_empty() {
        state.stage = Stage::Fish;
        write_state(round_dir, &state)?;
    }
    while let Some(joined) = set.join_next().await {
        let (name, retries, result) = joined?;
        let step = state.fish.get_mut(name).ok_or("unknown goldfish")?;
        step.retries += retries;
        match result {
            Ok(v) => {
                write_json(&fish_dir.join(format!("{name}.json")), &v)?;
                step.status = StepStatus::Done;
                log(format!("{name}: done"));
            }
            Err(e) => {
                step.status = StepStatus::Failed;
                step.error = Some(e.to_string());
                log(format!("{name}: failed: {e}"));
            }
        }
        write_state(round_dir, &state)?;
    }
    let failed: Vec<&String> = state.fish.iter().filter(|(_, s)| s.status != StepStatus::Done).map(|(n, _)| n).collect();
    if !failed.is_empty() {
        state.stage = Stage::Failed;
        write_state(round_dir, &state)?;
        return Err(format!("goldfish failed: {failed:?}; fix the cause, then run `egm resume`").into());
    }

    // Triage stage.
    let triage_path = round_dir.join("triage.json");
    if state.triage.status != StepStatus::Done {
        let rejections = collect_rejections(prd_dir, round)?;
        write_json(&round_dir.join("rejected-history.json"), &rejections)?;
        let rejected = if rejections.is_empty() { "无".to_string() } else { serde_json::to_string_pretty(&rejections)? };
        let fish_text = |n: &str| read(&fish_dir.join(format!("{n}.json")));
        let prompt = TRIAGE_PROMPT
            .replace("{{round}}", &round.to_string())
            .replace("{{elephant_rules}}", elephant_rules.trim())
            .replace("{{key_decisions}}", key_decisions.trim())
            .replace("{{rejected}}", &rejected)
            .replace("{{retell}}", fish_text("retell")?.trim())
            .replace("{{critic}}", fish_text("critic")?.trim())
            .replace("{{ready}}", fish_text("ready")?.trim())
            .replace("{{spec}}", fish_text("spec")?.trim())
            .replace("{{prd}}", prd.trim());
        let key_items = key_decisions.lines().filter(|l| l.starts_with("- ")).count();
        let claude = ClaudeConfig { model: settings.triager_model.clone() };

        state.stage = Stage::Triage;
        state.triage.status = StepStatus::Running;
        state.triage.error = None;
        write_state(round_dir, &state)?;
        log("triage: running".into());

        let mut result = Err::<TriageOutput, crate::Error>("not run".into());
        for attempt in 0..2 {
            result = match agents::claude(&claude, TRIAGE_SYSTEM, prompt.clone(), TRIAGE_SCHEMA).await {
                Ok(v) => validate_triage(&v, round, key_items),
                Err(e) => Err(e),
            };
            let retryable = matches!(&result, Err(e) if e.downcast_ref::<agents::ProcessError>().is_none());
            if result.is_ok() || !retryable {
                break;
            }
            state.triage.retries = attempt + 1;
        }
        match result {
            Ok(out) => {
                write_json(&triage_path, &Triage::from_output(round, out))?;
                state.triage.status = StepStatus::Done;
                state.stage = Stage::Decide;
                write_state(round_dir, &state)?;
                // First verdicts right away, so a round needing no decision
                // still reaches a conclusion (PRD §6.6).
                state.verdicts = Some(crate::review::refresh_verdicts(round_dir)?);
                log("triage: done".into());
            }
            Err(e) => {
                state.triage.status = StepStatus::Failed;
                state.triage.error = Some(e.to_string());
                state.stage = Stage::Failed;
                write_state(round_dir, &state)?;
                return Err(format!("triage failed: {e}; run `egm resume` to retry").into());
            }
        }
    }

    let triage: Triage = serde_json::from_str(&read(&triage_path)?)?;
    let verdicts = match state.verdicts {
        Some(v) => v,
        None => crate::review::refresh_verdicts(round_dir)?,
    };
    Ok(RoundResult { dir: round_dir.to_path_buf(), triage, verdicts, warnings: state.warnings })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Role, Severity};
    use crate::testutil::{dec, issue, triage_in, TempDir};

    fn save_round(prd_dir: &Path, n: u32, issues: Vec<crate::model::Issue>, decisions: Option<Vec<Decision>>) {
        let dir = prd_dir.join(format!("round-{n}"));
        fs::create_dir_all(&dir).unwrap();
        write_json(&dir.join("triage.json"), &triage_in(n, issues, 1, 1)).unwrap();
        if let Some(d) = decisions {
            write_json(&dir.join("decisions.json"), &d).unwrap();
        }
    }

    fn ids(r: &[Rejection]) -> Vec<&str> {
        r.iter().map(|x| x.id.as_str()).collect()
    }

    #[test]
    fn rejections_carry_over_until_the_author_accepts_a_match() {
        let tmp = TempDir::new("reject");
        let p = tmp.path();
        let i = |id: &str, prev: Option<&str>| issue(id, Role::Critic, &[], Severity::High, prev);
        let mut noted = dec("r1-01", Advice::Reject);
        noted.note = Some("设计如此".into());
        save_round(
            p,
            1,
            vec![i("r1-01", None), i("r1-02", None), i("r1-03", None)],
            Some(vec![noted, dec("r1-02", Advice::Accept), dec("r1-03", Advice::Reject)]),
        );
        let r = collect_rejections(p, 2).unwrap();
        assert_eq!(ids(&r), vec!["r1-01", "r1-03"]);
        assert_eq!(r[0].note.as_deref(), Some("设计如此"));

        // Round 2: r2-01 matched r1-01 and the author accepted it this time;
        // r2-02 matched r1-03 and was left alone (auto-rejected, no decision).
        save_round(
            p,
            2,
            vec![i("r2-01", Some("r1-01")), i("r2-02", Some("r1-03")), i("r2-03", None)],
            Some(vec![dec("r2-01", Advice::Accept), dec("r2-03", Advice::Reject)]),
        );
        assert_eq!(ids(&collect_rejections(p, 3).unwrap()), vec!["r1-03", "r2-03"]);
    }

    #[test]
    fn rounds_without_decisions_are_skipped() {
        let tmp = TempDir::new("nodec");
        let i = issue("r1-01", Role::Ready, &[], Severity::Low, None);
        save_round(tmp.path(), 1, vec![i], None);
        assert!(collect_rejections(tmp.path(), 2).unwrap().is_empty());
    }

    #[test]
    fn checklist_takes_its_section_and_drops_completion_item() {
        let rules = "## 参考\n\n- [ ] not this\n\n## 交付前检查清单\n\n- [ ] 有里程碑表\n- [ ] 末尾有关键决策清单\n- [ ] 项目做完后，标题加了 `[已完成]`\n\n## 参考\n- [ ] nor this\n";
        assert_eq!(spec_checklist(rules).unwrap(), "- 有里程碑表");
    }

    #[test]
    fn fish_validation_rejects_extra_fields_and_noteless_failures() {
        assert!(validate_fish("critic", r#"{"issues":[]}"#).is_ok());
        assert!(validate_fish("critic", r#"{"issues":[],"extra":1}"#).is_err());
        assert!(validate_fish("spec", r#"{"checks":[{"item":"a","pass":false,"note":""}]}"#).is_err());
        assert!(validate_fish("ready", r#"{"verdict":"GO","reason":"ok","missing":[]}"#).is_ok());
    }
}
