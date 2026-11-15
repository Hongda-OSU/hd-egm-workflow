//! Isolated runs of the goldfish (`codex exec`) and the triager
//! (`claude -p`). Flags and the reasons behind them: agent_docs/.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::Result;

const TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// Codex features that would let a goldfish see or touch anything beyond its
/// prompt (shell, JS host, apps incl. Notion, browser, ...).
const CODEX_DISABLED: [&str; 17] = [
    "shell_tool", "unified_exec", "code_mode_host", "apps", "plugins", "browser_use",
    "browser_use_external", "computer_use", "in_app_browser", "image_generation",
    "view_image", "multi_agent", "goals", "hooks", "skill_search", "tool_suggest", "sleep_tool",
];

/// A fresh temp dir outside any repo: `run/` is the agent's empty cwd, the
/// root holds our own files. Removed on drop.
struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new(label: &str) -> Result<Self> {
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let root = std::env::temp_dir().join(format!("egm-{label}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(root.join("run"))?;
        Ok(Scratch { root })
    }

    fn run_dir(&self) -> PathBuf {
        self.root.join("run")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Process-level failure (not installed, not logged in, quota, timeout).
/// Callers must not auto-retry these: a retry would just burn quota again.
#[derive(Debug)]
pub struct ProcessError(pub String);

impl std::fmt::Display for ProcessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ProcessError {}

fn tail(s: &str, max: usize) -> String {
    let s = s.trim();
    let start = s.char_indices().rev().nth(max).map_or(0, |(i, _)| i);
    s[start..].to_string()
}

async fn run(mut cmd: Command, input: String) -> Result<String> {
    let program = cmd.as_std().get_program().to_string_lossy().into_owned();
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    let mut child = cmd
        .spawn()
        .map_err(|e| ProcessError(format!("cannot start {program}: {e}")))?;
    let mut stdin = child.stdin.take().ok_or("no stdin")?;
    let writer = tokio::spawn(async move {
        let _ = stdin.write_all(input.as_bytes()).await;
    });
    let out = tokio::time::timeout(TIMEOUT, child.wait_with_output())
        .await
        .map_err(|_| ProcessError(format!("{program} timed out after {} min", TIMEOUT.as_secs() / 60)))??;
    let _ = writer.await;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let detail = if stderr.trim().is_empty() { stdout } else { stderr };
        return Err(ProcessError(format!("{program} exited with {}: {}", out.status, tail(&detail, 600))).into());
    }
    Ok(String::from_utf8(out.stdout)?)
}

#[derive(Debug, Clone)]
pub struct CodexConfig {
    pub home: PathBuf,
    pub model: String,
    pub effort: String,
}

/// Runs one goldfish; returns its final message (JSON text per `schema`).
pub async fn codex(cfg: &CodexConfig, prompt: String, schema: &str) -> Result<String> {
    let s = Scratch::new("fish")?;
    let schema_path = s.root.join("schema.json");
    let out_path = s.root.join("last.json");
    std::fs::write(&schema_path, schema)?;

    let mut cmd = Command::new("codex");
    cmd.current_dir(s.run_dir()).env("CODEX_HOME", &cfg.home).args([
        "exec", "--ephemeral", "--ignore-user-config", "--ignore-rules", "--skip-git-repo-check",
        "-s", "read-only", "-m", &cfg.model,
        "-c", &format!("model_reasoning_effort=\"{}\"", cfg.effort),
        "-c", "project_doc_max_bytes=0", "-c", "web_search=\"disabled\"",
    ]);
    for feature in CODEX_DISABLED {
        cmd.args(["--disable", feature]);
    }
    cmd.arg("--output-schema").arg(&schema_path).arg("-o").arg(&out_path).arg("-");
    run(cmd, prompt).await?;
    std::fs::read_to_string(&out_path)
        .map_err(|e| ProcessError(format!("codex wrote no final message: {e}")).into())
}

#[derive(Debug, Clone)]
pub struct ClaudeConfig {
    pub model: String,
}

/// Runs the triager; returns its structured output.
pub async fn claude(cfg: &ClaudeConfig, system: &str, prompt: String, schema: &str) -> Result<Value> {
    let s = Scratch::new("triage")?;
    // claude's validator rejects the draft 2020-12 `$schema` URI.
    let mut schema: Value = serde_json::from_str(schema)?;
    if let Some(o) = schema.as_object_mut() {
        o.remove("$schema");
    }
    let mut cmd = Command::new("claude");
    cmd.current_dir(s.run_dir()).args([
        "-p", "--model", &cfg.model, "--tools", "", "--strict-mcp-config",
        "--disable-slash-commands", "--no-session-persistence", "--setting-sources", "",
        "--system-prompt", system, "--json-schema", &schema.to_string(), "--output-format", "json",
    ]);
    let stdout = run(cmd, prompt).await?;
    let v: Value = serde_json::from_str(&stdout)?;
    if v["is_error"].as_bool() == Some(true) {
        return Err(ProcessError(format!("claude: {}", tail(v["result"].as_str().unwrap_or(""), 600))).into());
    }
    match v.get("structured_output") {
        Some(out) if !out.is_null() => Ok(out.clone()),
        _ => Err("claude returned no structured output".into()),
    }
}
