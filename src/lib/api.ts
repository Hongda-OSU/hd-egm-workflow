// Typed wrappers over the Tauri commands in src-tauri/src/main.rs.
// Shapes mirror egm-core's model.rs / review.rs / store.rs.
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export type Verdict = "GO" | "NO-GO";
export type Severity = "high" | "medium" | "low";
export type Role = "retell" | "critic" | "ready";
export type Advice = "accept" | "reject";

export interface Issue {
  id: string;
  source: Role;
  also_from: Role[];
  refs: string[];
  rejected_as: string | null;
  severity: Severity;
  section: string;
  title: string;
  problem: string;
  impact: string;
  advice: Advice;
  reason: string;
  suggested_change: string | null;
}

export interface Triage {
  round: number;
  key_decisions: { item: string; result: "hit" | "partial" | "miss" }[];
  key_decision_hits: number;
  key_decision_total: number;
  spec_checks: { item: string; pass: boolean }[];
  /** Goldfish entries left out of every issue; absent in older rounds. */
  discarded?: { ref: string; reason: string }[];
  issues: Issue[];
}

export interface Decision {
  issue_id: string;
  decision: Advice;
  note: string | null;
  decided_at: string;
}

export interface Step {
  status: "queued" | "running" | "done" | "failed";
  retries: number;
  error: string | null;
}

export interface RoundVerdicts {
  retell: Verdict;
  critic: Verdict;
  ready: Verdict;
  prd: Verdict;
}

export interface RoundView {
  n: number;
  latest: boolean;
  stage: string;
  fish_steps: Record<string, Step>;
  triage_step: Step;
  warnings: string[];
  triage: Triage | null;
  decisions: Decision[];
  verdicts: RoundVerdicts | null;
  has_handoff: boolean;
  handoff_stale: boolean;
  fish: Record<string, any>;
}

export interface PrdDetail {
  page_id: string;
  title: string;
  status: string;
  rounds: RoundView[];
}

export interface PrdItem {
  page_id: string;
  title: string;
  status: "none" | "in_review" | "go";
  rounds: number;
}

export interface PrdList {
  items: PrdItem[];
  offline: boolean;
  error: string | null;
}

export interface RuleItem {
  name: string;
  who: string;
  page_id: string | null;
}

export interface AppStatus {
  notion: boolean;
  codex: boolean;
  claude: boolean;
  codex_home: string | null;
  codex_version: string | null;
  claude_version: string | null;
  verified: { codex: string; claude: string };
}

export interface Models {
  goldfish_model: string;
  goldfish_effort: string;
  triager_model: string;
}

export interface SettingsView {
  models: Models;
  defaults: Models;
  data_dir: string;
  settings_path: string;
}

export const api = {
  appStatus: () => invoke<AppStatus>("app_status"),
  listPrds: () => invoke<PrdList>("list_prds"),
  rulePages: () => invoke<RuleItem[]>("rule_pages"),
  getPrd: (pageId: string) => invoke<PrdDetail | null>("get_prd", { pageId }),
  startRound: (pageId: string) => invoke<void>("start_round", { pageId }),
  resumeRound: (pageId: string) => invoke<void>("resume_round", { pageId }),
  decide: (pageId: string, round: number, issueId: string, decision: Advice, note: string | null) =>
    invoke<RoundVerdicts>("decide", { pageId, round, issueId, decision, note }),
  makeHandoff: (pageId: string, round: number) => invoke<string>("make_handoff", { pageId, round }),
  copyHandoff: (pageId: string, round: number) => invoke<void>("copy_handoff", { pageId, round }),
  copyText: (text: string) => invoke<void>("copy_text", { text }),
  openNotion: (pageId: string) => invoke<void>("open_notion", { pageId }),
  getSettings: () => invoke<SettingsView>("get_settings"),
  saveModels: (models: Models) => invoke<void>("save_models", { models }),
  reveal: (which: "data" | "settings") => invoke<void>("reveal", { which }),
};

export function onProgress(cb: (p: { page_id: string; line: string }) => void): Promise<UnlistenFn> {
  return listen<{ page_id: string; line: string }>("round-progress", (e) => cb(e.payload));
}

/** Resolves an issue ref like "critic#3" to that goldfish's original entry. */
export function resolveRef(fish: Record<string, any>, ref: string): string | null {
  const [name, idx] = ref.split("#");
  const i = Number(idx);
  const list =
    name === "retell" ? fish.retell?.guesses : name === "critic" ? fish.critic?.issues : name === "ready" ? fish.ready?.missing : null;
  const entry = Array.isArray(list) ? list[i] : null;
  if (!entry) return null;
  return entry.guess ?? entry.problem ?? entry.detail ?? JSON.stringify(entry);
}
