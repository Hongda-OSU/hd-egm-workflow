<script lang="ts">
  import { onMount, tick, untrack } from "svelte";
  import {
    api,
    onProgress,
    resolveRef,
    type Advice,
    type AppStatus,
    type DiffLine,
    type Issue,
    type IssueDiff,
    type Models,
    type Outline,
    type PrdDetail,
    type PrdItem,
    type PrdList,
    type Role,
    type RoundRef,
    type RuleItem,
    type SettingsView,
    type Step,
    type Verdict,
  } from "./lib/api";

  const ROLE: Record<Role, { label: string; color: string; bg: string }> = {
    retell: { label: "复述", color: "#4a5a7a", bg: "#eef1f6" },
    critic: { label: "找茬", color: "#b85414", bg: "#fbefe6" },
    ready: { label: "开工", color: "#1f6f8b", bg: "#e8f3f7" },
  };
  const SEV = {
    high: { label: "高", color: "#b3362b" },
    medium: { label: "中", color: "#a86b00" },
    low: { label: "低", color: "#5f6670" },
  };
  const ROLE_HINT: Record<Role, string> = {
    retell: "复述金鱼：读完用自己的话讲一遍，讲错说明 PRD 没写清楚",
    critic: "找茬金鱼：挑照着 PRD 做会做错的地方",
    ready: "开工金鱼：假装要照着 PRD 写代码，列出缺了什么",
  };
  const ROLES: Role[] = ["retell", "critic", "ready"];
  const foundBy = (i: Issue) => ROLES.filter((r) => i.source === r || i.also_from.includes(r));
  const FISH_NAMES: Record<string, string> = { retell: "复述", critic: "找茬", ready: "开工", spec: "规范" };
  // Shown only for a round that is not running now, so "running" means it
  // was cut off (app closed or crashed) rather than still going.
  const STEP_STATE: Record<Step["status"], string> = { queued: "没开始", running: "中断了", done: "完成", failed: "失败" };
  const STEP_ORDER = ["retell", "critic", "ready", "spec"];
  type StepRow = { name: string; status: Step["status"]; error: string | null };

  function stepsOf(r: PrdDetail["rounds"][number]): StepRow[] {
    const fish = Object.entries(r.fish_steps)
      .sort(([a], [b]) => STEP_ORDER.indexOf(a) - STEP_ORDER.indexOf(b))
      .map(([k, s]) => ({ name: `${FISH_NAMES[k] ?? k}金鱼`, status: s.status, error: s.error ?? null }));
    return [...fish, { name: "整理员", status: r.triage_step.status, error: r.triage_step.error ?? null }];
  }

  function unfinishedSummary(steps: StepRow[]): string {
    const names = (st: Step["status"]) => steps.filter((s) => s.status === st).map((s) => s.name).join("、");
    const parts = [
      names("failed") && `${names("failed")}失败了`,
      names("running") && `${names("running")}中断了`,
      names("queued") && `${names("queued")}还没开始`,
    ].filter(Boolean);
    return parts.length ? `${parts.join("，")}。` : "";
  }

  let status = $state<AppStatus | null>(null);
  let prds = $state<PrdList | null>(null);
  let rules = $state<RuleItem[]>([]);
  // True until the first PRD list (and the first PRD's rounds) arrive, so the
  // empty states never claim "nothing here" while Notion is still answering.
  let booting = $state(true);
  let selectedId = $state<string | null>(null);
  let detail = $state<PrdDetail | null>(null);
  // Which review of the selected PRD is shown; null is the current one.
  let viewReview = $state<number | null>(null);
  let reviewMenu = $state(false);
  let reviewConfirm = $state(false);
  let roundIdx = $state(0);
  let src = $state<"all" | Role>("all");
  let issueId = $state<string | null>(null);
  let note = $state("");
  let noteOpen = $state(false);
  let noteEl = $state<HTMLInputElement | null>(null);
  // Note saves and decisions go out one at a time so a note saved on blur
  // can never land after (and undo) the decision click that caused the blur.
  let saving: Promise<void> = Promise.resolve();
  let running = $state<string | null>(null);
  let progressLines = $state<string[]>([]);
  // Live view of a running round, built from the progress lines the backend
  // emits ("retell: running", "critic: done", "triage: failed: …").
  type LiveStep = { status: Step["status"]; start: number | null; end: number | null; error: string | null };
  const FISH_KEYS = ["retell", "critic", "ready", "spec"];
  let live = $state<Record<string, LiveStep>>({});
  /** Seconds the pull waits for a just-made Notion edit to settle; null when not waiting. */
  let pullWait = $state<number | null>(null);
  let liveLabel = $state("");
  let runStart = $state(0);
  let now = $state(Date.now());
  let message = $state<{ text: string; error: boolean } | null>(null);

  // Preflight: what a round needs. Nothing is shown while everything passes.
  type Problem = { text: string; cmd: string | null };
  const versionRows = $derived(
    status
      ? [
          { name: "codex", have: status.codex_version, want: status.verified.codex },
          { name: "claude", have: status.claude_version, want: status.verified.claude },
        ]
      : [],
  );
  const problems = $derived.by((): Problem[] => {
    if (!status) return [];
    const out: Problem[] = [];
    if (!status.notion)
      out.push({ text: "钥匙串里没有 Notion token，拉不到 PRD。在终端运行", cmd: "security add-generic-password -s hd-egm-workflow -a notion -w" });
    if (!status.codex)
      out.push(status.codex_home
        ? { text: "GPT 金鱼没登录，审稿跑不了。在终端运行", cmd: `CODEX_HOME="${status.codex_home}" codex login` }
        : { text: "读不到 app 的设置文件，GPT 金鱼没法启动。", cmd: null });
    if (!status.claude)
      out.push({ text: "Claude 整理员没登录，整理那一步跑不了。在终端运行", cmd: "claude auth login" });
    for (const v of versionRows)
      if (v.have && v.have !== v.want)
        out.push({
          text: `${v.name} 现在是 ${v.have}，隔离只在 ${v.want} 上测过。先按 agent_docs/ 重测，通过后把 settings.json 的 verified_cli_versions 改成新版本，再点右边重新检查。`,
          cmd: null,
        });
    return out;
  });
  const blocked = $derived(problems.length > 0);
  let checking = $state(false);

  async function checkStatus(): Promise<boolean> {
    if (checking) return !blocked;
    checking = true;
    try {
      status = await api.appStatus();
    } catch (e) {
      flash(String(e), true);
    } finally {
      checking = false;
    }
    return !blocked;
  }

  const selectedItem = $derived(prds?.items.find((p) => p.page_id === selectedId) ?? null);

  // Notion titles carry an archive prefix ("[已完成] ") and a doc-type suffix
  // (" · 产品文档"); the list shows them as groups and drops the suffix.
  type Group = "active" | "done" | "dropped";
  const GROUPS: { key: Group; label: string }[] = [
    { key: "active", label: "进行中" },
    { key: "done", label: "已完成" },
    { key: "dropped", label: "已放弃" },
  ];
  function parseTitle(title: string): { name: string; group: Group } {
    const m = title.match(/^\s*\[(已完成|已放弃)\]\s*/);
    const group: Group = m ? (m[1] === "已完成" ? "done" : "dropped") : "active";
    const name = title.slice(m?.[0].length ?? 0).replace(/\s*[·—–-]\s*产品文档\s*$/, "").trim();
    return { name: name || title, group };
  }
  let query = $state("");
  let openGroups = $state<Record<Group, boolean>>({ active: true, done: false, dropped: false });
  const grouped = $derived.by(() => {
    const q = query.trim().toLowerCase();
    const out: Record<Group, (PrdItem & { name: string })[]> = { active: [], done: [], dropped: [] };
    for (const p of prds?.items ?? []) {
      const { name, group } = parseTitle(p.title);
      if (!q || name.toLowerCase().includes(q)) out[group].push({ ...p, name });
    }
    return out;
  });
  const round = $derived(detail?.rounds[roundIdx] ?? null);
  const triage = $derived(round?.triage ?? null);
  const decisionOf = (id: string): Advice | null =>
    round?.decisions.find((d) => d.issue_id === id)?.decision ?? null;
  // Order only by things a decision cannot change (auto-rejected last, then
  // severity), so the list never reshuffles while the user works through it.
  const SEVERITY_RANK: Record<string, number> = { high: 0, medium: 1, low: 2 };
  const rank = (i: Issue) => (i.rejected_as ? 3 : 0) + (SEVERITY_RANK[i.severity] ?? 2) / 10;
  const shown = $derived(
    triage
      ? triage.issues
          .filter((i) => src === "all" || i.source === src || i.also_from.includes(src))
          .map((i, n) => ({ i, n }))
          .sort((a, b) => rank(a.i) - rank(b.i) || a.n - b.n)
          .map(({ i }) => i)
      : [],
  );
  const cur = $derived(triage?.issues.find((i) => i.id === issueId) ?? shown[0] ?? null);
  const undecided = $derived(
    triage ? triage.issues.filter((i) => !i.rejected_as && decisionOf(i.id) === null) : [],
  );
  // Earlier reviews are read-only history: no starting, deciding or resuming.
  const viewingCurrent = $derived(!!detail && detail.review === detail.current_review);
  // Once the latest round is handed off, starting the next one becomes the
  // page's main action; before that, deciding issues is.
  const readyForNext = $derived(viewingCurrent && !!detail?.rounds.at(-1)?.has_handoff);
  const isGo = $derived(viewingCurrent && detail?.status === "go");
  const nextRound = $derived((detail?.rounds.at(-1)?.n ?? 0) + 1);
  const unfinished = $derived(
    round && round.latest && !round.triage && (round.stage === "failed" || round.stage === "fish" || round.stage === "triage"),
  );
  const maxIssues = $derived(Math.max(1, ...(detail?.rounds.map((r) => r.triage?.issues.length ?? 0) ?? [1])));

  $effect(() => {
    const id = cur?.id;
    note = (id && round?.decisions.find((d) => d.issue_id === id)?.note) || "";
    noteOpen = false;
  });
  // The note stays folded away unless it has text or the issue is rejected,
  // where it doubles as the rejection reason.
  const showNote = $derived(noteOpen || note.trim() !== "" || (!!cur && decisionOf(cur.id) === "reject"));
  const CHOICES: Advice[] = ["accept", "reject"];
  const word = (a: Advice) => (a === "accept" ? "接受" : "驳回");
  const sectionsOf = (i: Issue) => i.section.split(/\s*[；;]\s*/).join(" · ");

  async function openNote() {
    noteOpen = true;
    await tick();
    noteEl?.focus();
  }

  function saveNote() {
    saving = saving.then(async () => {
      if (!detail || !round || !cur || !round.latest) return;
      const d = decisionOf(cur.id);
      const saved = round.decisions.find((x) => x.issue_id === cur!.id)?.note ?? "";
      // Without a decision the note just waits and goes out with the click.
      if (!d || note.trim() === saved.trim()) return;
      try {
        await api.decide(detail.page_id, detail.review, round.n, cur.id, d, note.trim() || null);
        await loadDetail(true);
      } catch (e) {
        flash(String(e), true);
      }
    });
    return saving;
  }

  // Settings dialog: only the model fields are editable; they take effect
  // from the next round.
  const EFFORTS = ["low", "medium", "high"];
  const MODEL_KEYS = ["goldfish_model", "goldfish_effort", "triager_model"] as const;
  let settingsView = $state<SettingsView | null>(null);
  let settingsOpen = $state(false);
  let draft = $state<Models>({ goldfish_model: "", goldfish_effort: "", triager_model: "" });
  let savingModels = $state(false);
  const dirty = $derived(!!settingsView && MODEL_KEYS.some((k) => draft[k].trim() !== settingsView!.models[k]));
  const canSave = $derived(dirty && MODEL_KEYS.every((k) => draft[k].trim() !== ""));
  // Neither CLI can list the models an account may use, so the menus hold
  // names known to exist; anything else goes through "其他…".
  const OTHER = "__other__";
  const MODEL_CHOICES = {
    goldfish_model: ["gpt-6.1-sol"],
    triager_model: ["claude-opus-5-5", "claude-sonnet-5-5", "claude-haiku-5-5", "claude-fable-5-2"],
  };
  type ModelKey = keyof typeof MODEL_CHOICES;
  let custom = $state<Record<ModelKey, boolean>>({ goldfish_model: false, triager_model: false });
  let customEls = $state<Record<ModelKey, HTMLInputElement | null>>({ goldfish_model: null, triager_model: null });
  const choicesOf = (k: ModelKey) => {
    const d = settingsView?.defaults[k];
    return d && !MODEL_CHOICES[k].includes(d) ? [d, ...MODEL_CHOICES[k]] : MODEL_CHOICES[k];
  };

  async function pickModel(k: ModelKey, v: string) {
    if (v === OTHER) {
      custom[k] = true;
      draft[k] = "";
      await tick();
      customEls[k]?.focus();
    } else {
      custom[k] = false;
      draft[k] = v;
    }
  }

  function restoreDefault(k: keyof Models) {
    draft[k] = settingsView?.defaults[k] ?? draft[k];
    if (k !== "goldfish_effort") custom[k] = false;
  }
  const effortOptions = $derived(
    EFFORTS.includes(draft.goldfish_effort) ? EFFORTS : [...EFFORTS, draft.goldfish_effort],
  );

  async function openSettings() {
    try {
      settingsView = await api.getSettings();
      draft = { ...settingsView.models };
      for (const k of ["goldfish_model", "triager_model"] as const) custom[k] = !choicesOf(k).includes(draft[k]);
      settingsOpen = true;
      await tick();
      document.getElementById("goldfish-model")?.focus();
    } catch (e) {
      flash(String(e), true);
    }
  }

  async function saveSettings() {
    if (!canSave || savingModels) return;
    savingModels = true;
    try {
      await api.saveModels({
        goldfish_model: draft.goldfish_model.trim(),
        goldfish_effort: draft.goldfish_effort.trim(),
        triager_model: draft.triager_model.trim(),
      });
      settingsOpen = false;
      flash("模型已保存，从下一轮开始生效");
    } catch (e) {
      flash(String(e), true);
    } finally {
      savingModels = false;
    }
  }

  function flash(text: string, error = false) {
    message = { text, error };
    setTimeout(() => (message = null), error ? 8000 : 3000);
  }

  function stateOf(i: Issue): { text: string; color: string } {
    const d = decisionOf(i.id);
    if (d === "accept") return { text: "已接受", color: "#1f7a4d" };
    if (d === "reject") return { text: "已驳回", color: "#5f6670" };
    if (i.rejected_as) return { text: `自动驳回 · 同 ${i.rejected_as}`, color: "#5f6670" };
    return { text: "待我决定", color: "#b85414" };
  }

  // PRD §6.6; must match HIT_THRESHOLD in egm-core's review.rs.
  const HIT_THRESHOLD = 0.9;
  const hitRate = (t: { key_decision_hits: number; key_decision_total: number }) =>
    t.key_decision_total ? t.key_decision_hits / t.key_decision_total : 0;
  const pct = (x: number) => `${Math.round(x * 100)}%`;

  // Same rule as egm-core: a manual decision wins, else an earlier rejection.
  const isRejected = (i: Issue) => {
    const d = decisionOf(i.id);
    return d ? d === "reject" : !!i.rejected_as;
  };

  // Why each line is not GO, in the terms of §6.6: unrejected highs counted
  // toward it (split into ones already accepted and ones still open) and,
  // for retell, the key-decision hit rate.
  const lines = $derived.by(() => {
    if (!triage || !round) return [];
    const rate = hitRate(triage);
    return ROLES.map((role) => {
      const highs = triage.issues.filter((i) => i.severity === "high" && foundBy(i).includes(role) && !isRejected(i));
      const toFix = highs.filter((i) => decisionOf(i.id) === "accept").length;
      const open = highs.length - toFix;
      const reasons: string[] = [];
      if (toFix) reasons.push(`${toFix} 条高严重度要改`);
      if (open) reasons.push(`${open} 条高严重度待决定`);
      if (role === "retell" && rate < HIT_THRESHOLD) reasons.push(`命中率 ${pct(rate)}，不到 ${pct(HIT_THRESHOLD)}`);
      return { role, verdict: round.verdicts?.[role], reasons, open, toFix };
    });
  });

  const verdictNote = $derived.by(() => {
    const v = round?.verdicts?.prd;
    if (!v) return "";
    if (v === "GO") return "三条线都过了，这份 PRD 可以收尾";
    if (lines.some((l) => l.open)) return "先把高严重度的问题拍完";
    if (lines.some((l) => l.toFix)) return "把接受的问题改进 PRD，下一轮再看";
    if (lines.some((l) => l.reasons.length)) return "关键决策没讲对，把 PRD 写清楚再审一轮";
    if (round?.warnings.length) return "有块转换警告，见下方";
    return "";
  });

  function specNote(fish: Record<string, any>, item: string): string {
    const c = fish.spec?.checks?.find((x: { item: string }) => x.item === item);
    return typeof c?.note === "string" ? c.note : "";
  }

  /** Renders **bold** and `code` from rule text; everything else is escaped. */
  function inlineMd(text: string): string {
    const esc = text.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
    return esc.replace(/\*\*(.+?)\*\*/g, "<b>$1</b>").replace(/`(.+?)`/g, "<code>$1</code>");
  }

  function verdictStyle(v: Verdict | undefined) {
    if (!v) return "color:#5f6670;background:#ecebe5";
    return v === "GO" ? "color:#1f7a4d;background:#e6f4ec" : "color:#b3362b;background:#fbeae8";
  }

  function roundSummary(r: PrdDetail["rounds"][number]) {
    if (!r.triage) return r.stage === "failed" ? "没跑完" : "只拉了快照";
    const t = r.triage;
    return `${t.issues.length} 个问题 · 命中 ${t.key_decision_hits}/${t.key_decision_total}`;
  }

  async function loadList() {
    try {
      prds = await api.listPrds();
      if (!selectedId && prds.items.length) await select(prds.items[0].page_id);
    } catch (e) {
      flash(String(e), true);
    } finally {
      booting = false;
    }
  }

  // Rule pages come from local settings, so they need not wait for Notion.
  async function loadRules() {
    try {
      rules = await api.rulePages();
    } catch (e) {
      flash(String(e), true);
    }
  }

  function refresh() {
    loadRules();
    loadList();
  }

  async function loadDetail(keepPosition: boolean) {
    const id = selectedId;
    if (!id) return;
    const loaded = await api.getPrd(id, viewReview);
    // Clicking through PRDs quickly can resolve loads out of order; a reply
    // for a PRD that is no longer selected must not replace the current one.
    if (selectedId !== id) return;
    detail = loaded;
    const last = Math.max(0, (detail?.rounds.length ?? 1) - 1);
    if (!keepPosition || roundIdx > last) roundIdx = last;
  }

  // Before a PRD's first round: its sections and key-decision check, read
  // from Notion so a missing list shows before "start" is clicked.
  let outline = $state<{ id: string; data: Outline | null; error: string | null } | null>(null);
  const fresh = $derived(!!selectedItem && !booting && (!detail || detail.rounds.length === 0));
  const keyBad = $derived(!!outline && outline.id === selectedId && !!outline.data?.key_problem);
  const KEY_PROBLEM = {
    missing: { text: "缺「关键决策清单」", hint: "让写 PRD 的 Claude 补上，只列正文里写明的决定" },
    duplicate: { text: "「关键决策清单」出现了不止一次", hint: "让写 PRD 的 Claude 只保留一节" },
    empty: { text: "「关键决策清单」是空的", hint: "让写 PRD 的 Claude 补上条目，只列正文里写明的决定" },
  };

  async function loadOutline(id: string) {
    outline = { id, data: null, error: null };
    try {
      const data = await api.prdOutline(id);
      if (selectedId === id) outline = { id, data, error: null };
    } catch (e) {
      if (selectedId === id) outline = { id, data: null, error: String(e) };
    }
  }

  $effect(() => {
    const id = selectedId;
    if (fresh && id && untrack(() => outline?.id) !== id) loadOutline(id);
  });

  function recheckFresh() {
    checkStatus();
    if (selectedId) loadOutline(selectedId);
  }

  function keyPrompt(problem: keyof typeof KEY_PROBLEM, title: string): string {
    const only = "只列正文里已经写明的决定，不要新增决定，也不要改正文。";
    if (problem === "duplicate") return `「${title}」里「关键决策清单」这个标题出现了不止一次。请只保留末尾那一节，把其他的内容合并进去，不要改正文。`;
    if (problem === "empty")
      return `「${title}」末尾的「关键决策清单」是空的。请用圆点列表补上这份 PRD 里已经做出的关键决定，每条一句话。${only}`;
    return `请在「${title}」末尾加一节「关键决策清单」：每条一句话、用圆点列表，列出这份 PRD 里已经做出的关键决定（技术选型、范围取舍、数据怎么存和同步、明确不做的事等）。${only}`;
  }

  async function copyKeyPrompt(problem: keyof typeof KEY_PROBLEM) {
    try {
      await api.copyText(keyPrompt(problem, selectedItem?.title ?? ""));
      flash("已复制，贴给写 PRD 的 Claude");
    } catch (e) {
      flash(String(e), true);
    }
  }

  async function select(id: string) {
    selectedId = id;
    viewReview = null;
    reviewMenu = false;
    issueId = null;
    src = "all";
    try {
      await loadDetail(false);
    } catch (e) {
      flash(String(e), true);
    }
  }

  function onLiveLine(line: string) {
    const t = Date.now();
    if (line.startsWith("pull:")) {
      const w = line.match(/^pull: waiting (\d+)s/);
      if (w) pullWait = Number(w[1]);
      else if (live.pull) live.pull = { ...live.pull, status: "running", start: t };
      return;
    }
    const m = line.match(/^(retell|critic|ready|spec|triage): (running|done|failed)(?::\s*(.*))?$/);
    if (!m) return;
    const [, key, status, error] = m;
    // The first goldfish starting means the snapshot is in.
    if (live.pull?.status === "running") live.pull = { ...live.pull, status: "done", end: t };
    const prev = live[key] ?? { status: "queued", start: null, end: null, error: null };
    live[key] =
      status === "running"
        ? { ...prev, status: "running", start: t, end: null, error: null }
        : { ...prev, status: status as Step["status"], end: t, error: error ?? null };
  }

  const clock = (ms: number) => {
    const s = Math.max(0, Math.floor(ms / 1000));
    return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
  };
  const took = (st: LiveStep | undefined) =>
    st?.start ? clock((st.end ?? now) - st.start) : "";

  const liveFish = $derived(FISH_KEYS.map((k) => ({ key: k, name: `${FISH_NAMES[k]}金鱼`, st: live[k] })));
  const fishDone = $derived(liveFish.filter((f) => f.st?.status === "done").length);
  const fishSpan = $derived.by(() => {
    const starts = liveFish.map((f) => f.st?.start).filter((x): x is number => !!x);
    const ends = liveFish.map((f) => f.st?.end).filter((x): x is number => !!x);
    return starts.length && ends.length ? clock(Math.max(...ends) - Math.min(...starts)) : "";
  });
  const liveHead = $derived(
    live.triage?.status === "running"
      ? "正在归类"
      : live.pull?.status === "running"
        ? "正在拉取快照"
        : `正在审稿 · ${fishDone} / ${FISH_KEYS.length} 条金鱼完成`,
  );

  async function run(kind: "start" | "resume" | "review") {
    if (!selectedId || running) return;
    if (!(await checkStatus())) return;
    running = selectedId;
    progressLines = [];
    pullWait = null;
    runStart = now = Date.now();
    const last = viewingCurrent ? detail?.rounds.at(-1) : undefined;
    const fresh = (): LiveStep => ({ status: "queued", start: null, end: null, error: null });
    const curReview = detail?.current_review ?? 1;
    const reviewPrefix = (k: number) => (k > 1 ? `第 ${k} 次审稿 · ` : "");
    if (kind === "review") {
      liveLabel = `${reviewPrefix(curReview + 1)}第 1 轮`;
      live = Object.fromEntries(["pull", ...FISH_KEYS, "triage"].map((k) => [k, fresh()]));
    } else if (kind === "start" || !last) {
      liveLabel = `${reviewPrefix(curReview)}第 ${(last?.n ?? 0) + 1} 轮`;
      live = Object.fromEntries(["pull", ...FISH_KEYS, "triage"].map((k) => [k, fresh()]));
    } else {
      // Resume skips finished steps and emits nothing for them.
      liveLabel = `${reviewPrefix(curReview)}第 ${last.n} 轮`;
      live = Object.fromEntries([
        ...FISH_KEYS.map((k) => [k, { ...fresh(), status: last.fish_steps[k]?.status === "done" ? "done" : "queued" }]),
        ["triage", fresh()],
      ]);
    }
    const tick = setInterval(() => (now = Date.now()), 1000);
    try {
      await (kind === "start"
        ? api.startRound(selectedId)
        : kind === "review"
          ? api.startReview(selectedId)
          : api.resumeRound(selectedId));
      flash("这一轮跑完了");
    } catch (e) {
      flash(String(e), true);
    } finally {
      clearInterval(tick);
      running = null;
      issueId = null;
      viewReview = null;
      await loadDetail(false);
      await loadList();
    }
  }

  async function decide(d: Advice) {
    await saving;
    if (!detail || !round || !cur) return;
    const id = cur.id;
    // Only a first-time accept moves on; a reject stays put so the reason
    // can be typed, and changing an earlier decision stays put too.
    const advance = d === "accept" && decisionOf(id) === null && !cur.rejected_as;
    try {
      await api.decide(detail.page_id, detail.review, round.n, id, d, note.trim() || null);
      const at = shown.findIndex((i) => i.id === id);
      await loadDetail(true);
      issueId = id;
      if (advance) {
        const open = (i: Issue) => !i.rejected_as && decisionOf(i.id) === null;
        const next = shown.slice(at + 1).find(open) ?? shown.slice(0, at).find(open);
        if (next) issueId = next.id;
      } else if (d === "reject") {
        await tick();
        noteEl?.focus();
      }
      await loadList();
    } catch (e) {
      flash(String(e), true);
    }
  }

  async function handoff() {
    if (!detail || !round) return;
    try {
      await api.makeHandoff(detail.page_id, detail.review, round.n);
      flash("交接文档已生成并复制到剪贴板，贴给大象即可");
      await loadDetail(true);
    } catch (e) {
      flash(String(e), true);
    }
  }

  async function copyHandoff() {
    if (!detail || !round) return;
    try {
      await api.copyHandoff(detail.page_id, detail.review, round.n);
      flash("交接文档已复制到剪贴板");
    } catch (e) {
      flash(String(e), true);
    }
  }

  function pickReview(n: number) {
    reviewMenu = false;
    if (!detail || n === detail.review) return;
    viewReview = n === detail.current_review ? null : n;
    issueId = null;
    src = "all";
    loadDetail(false).catch((e) => flash(String(e), true));
  }

  function startReview() {
    reviewConfirm = false;
    run("review");
  }

  // Section diffs under an issue (M5), loaded per issue and keyed, so a
  // slow reply for an issue no longer shown is dropped.
  let issueDiff = $state<{ key: string; data: IssueDiff | null; error: string | null } | null>(null);
  const roundKey = $derived(detail && round ? `${detail.page_id}|${detail.review}|${round.n}` : "");
  // The round count is part of the key: a new round gives older ones a "next".
  const issueKey = $derived(
    detail && round && cur ? `${roundKey}|${cur.id}|${detail.current_review}|${detail.rounds.length}` : "",
  );
  const hasNext = $derived(!!detail && !!round && (round !== detail.rounds.at(-1) || !viewingCurrent));

  async function loadIssueDiff(key: string, pageId: string, review: number, n: number, section: string) {
    issueDiff = { key, data: null, error: null };
    try {
      const data = await api.issueDiff(pageId, review, n, section);
      if (issueKey === key) issueDiff = { key, data, error: null };
    } catch (e) {
      if (issueKey === key) issueDiff = { key, data: null, error: String(e) };
    }
  }

  $effect(() => {
    const key = issueKey;
    if (key && hasNext && detail && round && cur && untrack(() => issueDiff?.key) !== key)
      loadIssueDiff(key, detail.page_id, detail.review, round.n, cur.section);
  });

  const roundLabel = (r: RoundRef) =>
    detail && r.review !== detail.review ? `第 ${r.review} 次审稿第 ${r.round} 轮` : `第 ${r.round} 轮`;
  const changeCount = (a: number, r: number) => [a && `+${a}`, r && `−${r}`].filter(Boolean).join(" ") || "±0";

  function closeReviewMenu(e: MouseEvent) {
    if (reviewMenu && !(e.target as Element | null)?.closest(".review-pick")) reviewMenu = false;
  }

  // With many rounds the tab row scrolls; keep the selected one in view.
  $effect(() => {
    void roundIdx;
    void detail;
    tick().then(() => document.querySelector(".tab.on")?.scrollIntoView({ block: "nearest", inline: "nearest" }));
  });

  onMount(() => {
    const unlisten = onProgress((p) => {
      if (p.page_id !== running) return;
      progressLines = [...progressLines, p.line];
      onLiveLine(p.line);
    });
    checkStatus();
    refresh();
    // Coming back from a terminal login should clear the banner by itself.
    const onFocus = () => checkStatus();
    window.addEventListener("focus", onFocus);
    return () => {
      unlisten.then((f) => f());
      window.removeEventListener("focus", onFocus);
    };
  });
</script>

{#snippet liveRow(name: string, st: LiveStep | undefined, indent: boolean, time: string, runningHint = "")}
  {@const status = st?.status ?? "queued"}
  <div class="step" class:indent>
    <div class="step-row">
      <span class="step-mark {status}" aria-hidden="true">
        {#if status === "done"}
          <svg width="12" height="12" viewBox="0 0 12 12"><path d="M2.5 6.2l2.3 2.3 4.7-5" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" /></svg>
        {:else if status === "running"}
          <span class="spinner"></span>
        {:else}<span class="dot"></span>{/if}
      </span>
      <span class="step-name">{name}</span>
      <span class="step-status {status}">
        {status === "running" ? (name === "整理员归类" ? "归类中…" : name.endsWith("快照") ? "拉取中…" : "审稿中…")
          : status === "done" ? "完成" : status === "failed" ? "失败"
          : name === "整理员归类" ? "等金鱼都审完" : "排队"}
      </span>
      <span class="step-time">{time || took(st)}</span>
    </div>
    {#if status === "running" && runningHint}<div class="step-error muted">{runningHint}</div>{/if}
    {#if st?.error}<div class="step-error mono">{st.error}</div>{/if}
  </div>
{/snippet}

<svelte:window onclick={closeReviewMenu} onkeydown={(e) => e.key === "Escape" && (reviewMenu = false)} />

{#snippet diffLines(lines: DiffLine[])}
  {#each lines as l, i (i)}
    {#if l.kind === "gap"}
      <div class="dl gap">⋯ {l.count} 行没变</div>
    {:else}
      <div class="dl {l.kind}"><span class="dl-mark" aria-hidden="true">{l.kind === "add" ? "+" : l.kind === "del" ? "−" : ""}</span><span class="dl-text">{l.text}</span></div>
    {/if}
  {/each}
{/snippet}

{#snippet dots()}
  <span class="dots" aria-hidden="true"><span></span><span></span><span></span></span>
{/snippet}

<div class="app">
  <header class="top">
    <div class="brand">
      <span class="name">hd-egm-workflow</span>
      <span class="muted">Claude 写 PRD，GPT 零上下文审稿</span>
    </div>
    <button type="button" class="icon-btn" onclick={openSettings} title="设置" aria-label="设置">
      <svg width="15" height="15" viewBox="0 0 16 16" aria-hidden="true" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round">
        <circle cx="8" cy="8" r="2.2" />
        <path d="M8 1.5v2M8 12.5v2M1.5 8h2M12.5 8h2M3.4 3.4l1.4 1.4M11.2 11.2l1.4 1.4M3.4 12.6l1.4-1.4M11.2 4.8l1.4-1.4" />
      </svg>
    </button>
  </header>

  {#if blocked}
    <div class="preflight" role="alert">
      <div class="preflight-lines">
        {#each problems as p}
          <p><span class="bad-dot" aria-hidden="true"></span><span><b>{p.text}</b>{#if p.cmd}<code>{p.cmd}</code>，然后点右边重新检查。{/if}</span></p>
        {/each}
      </div>
      <button type="button" class="recheck" disabled={checking} onclick={checkStatus}>{checking ? "检查中…" : "重新检查"}</button>
    </div>
  {/if}

  {#snippet backToDefault(key: keyof Models)}
    {@const d = settingsView?.defaults[key] ?? ""}
    {#if draft[key].trim() !== d}
      · <span class="changed-text">默认是 {d}</span> ·
      <button type="button" class="link-btn" onclick={() => restoreDefault(key)}>恢复默认</button>
    {/if}
  {/snippet}

  {#snippet modelPicker(key: ModelKey, id: string, cli: string, base: string)}
    {@const d = settingsView?.defaults[key] ?? ""}
    <select {id} class="text-in select" class:changed={draft[key].trim() !== d}
      value={custom[key] ? OTHER : draft[key]} onchange={(e) => pickModel(key, e.currentTarget.value)}>
      {#each choicesOf(key) as m (m)}
        <option value={m}>{m}{m === d ? "（默认）" : ""}</option>
      {/each}
      <option value={OTHER}>其他…</option>
    </select>
    {#if custom[key]}
      <input class="text-in custom-in" bind:this={customEls[key]} bind:value={draft[key]}
        aria-label="{cli} 模型名" placeholder="{cli} 支持的模型名" spellcheck="false" autocomplete="off" />
    {/if}
    {#if base || custom[key] || draft[key].trim() !== d}
      <div class="hint">{custom[key] ? `填 ${cli} 支持的模型名` : base} {@render backToDefault(key)}</div>
    {/if}
  {/snippet}

  {#if settingsOpen && settingsView}
    <!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
    <div class="backdrop" onclick={(e) => e.target === e.currentTarget && (settingsOpen = false)}
      onkeydown={(e) => e.key === "Escape" && (settingsOpen = false)}>
      <div class="dialog" role="dialog" aria-modal="true" aria-labelledby="settings-title">
        <div class="dialog-head">
          <h2 id="settings-title">设置</h2>
          <button type="button" class="icon-btn" onclick={() => (settingsOpen = false)} aria-label="关闭">✕</button>
        </div>

        <section class="dialog-sec">
          <div class="label">模型</div>
          <div class="field">
            <label for="goldfish-model">金鱼模型</label>
            <div>
              {@render modelPicker("goldfish_model", "goldfish-model", "codex", "4 条金鱼共用这一个模型")}
            </div>
          </div>
          <div class="field">
            <span class="field-label" id="effort-label">金鱼推理强度</span>
            <div>
              <div class="seg" role="radiogroup" aria-labelledby="effort-label">
                {#each effortOptions as o (o)}
                  <button type="button" role="radio" aria-checked={draft.goldfish_effort === o} class:on={draft.goldfish_effort === o}
                    onclick={() => (draft.goldfish_effort = o)}>{o}</button>
                {/each}
              </div>
              {#if draft.goldfish_effort !== settingsView.defaults.goldfish_effort}
                <div class="hint">{@render backToDefault("goldfish_effort")}</div>
              {/if}
            </div>
          </div>
          <div class="field">
            <label for="triager-model">整理员模型</label>
            <div>
              {@render modelPicker("triager_model", "triager-model", "claude", "")}
            </div>
          </div>
          {#if dirty}
            <p class="model-warn">换了模型，从下一轮开始生效。之前测过的隔离不一定还成立，建议按 agent_docs/ 重测一次；新一轮的结果也跟之前几轮不能直接比。</p>
          {:else}
            <p class="muted small-text model-note">从下一轮开始生效，已经跑完的轮次不受影响。</p>
          {/if}
        </section>

        <section class="dialog-sec">
          <div class="label">命令行工具 <span class="label-note">只读，启动时自动检查</span></div>
          {#each versionRows as v (v.name)}
            <div class="ver-row">
              <span>{v.name}</span>
              <span class="mono">{v.have ?? "—"}</span>
              {#if !v.have}
                <span class="bad-text">没找到这个命令</span>
              {:else if v.have === v.want}
                <span class="ok-text">✓ 跟隔离实测的版本一致</span>
              {:else}
                <span class="bad-text">实测的是 {v.want}，要先按 agent_docs/ 重测</span>
              {/if}
            </div>
          {:else}
            <p class="muted small-text">还在检查…</p>
          {/each}
        </section>

        <section class="dialog-sec">
          <div class="label">文件 <span class="label-note">只读</span></div>
          <div class="file-row">
            <span>运行数据</span><span class="mono ellipsis" title={settingsView.data_dir}>{settingsView.data_dir}</span>
            <button type="button" class="link-btn" onclick={() => api.reveal("data").catch((e) => flash(String(e), true))}>在 Finder 中显示</button>
          </div>
          <div class="file-row">
            <span>设置文件</span><span class="mono ellipsis" title={settingsView.settings_path}>{settingsView.settings_path}</span>
            <button type="button" class="link-btn" onclick={() => api.reveal("settings").catch((e) => flash(String(e), true))}>在 Finder 中显示</button>
          </div>
        </section>

        <div class="dialog-foot">
          <button type="button" class="secondary" onclick={() => (settingsOpen = false)}>取消</button>
          <button type="button" class="secondary strong" disabled={!canSave || savingModels} onclick={saveSettings}>
            {savingModels ? "保存中…" : "保存"}
          </button>
        </div>
      </div>
    </div>
  {/if}

  {#if reviewConfirm && detail}
    {@const last = detail.reviews.at(-1)}
    <!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
    <div class="backdrop" onclick={(e) => e.target === e.currentTarget && (reviewConfirm = false)}
      onkeydown={(e) => e.key === "Escape" && (reviewConfirm = false)}>
      <div class="dialog confirm" role="dialog" aria-modal="true" aria-labelledby="review-title">
        <div class="dialog-head">
          <h2 id="review-title">重新审一遍「{parseTitle(detail.title).name}」？</h2>
          <button type="button" class="icon-btn" onclick={() => (reviewConfirm = false)} aria-label="关闭">✕</button>
        </div>
        <ul class="confirm-list">
          <li>从 Notion 拉最新版本，开始<b>第 {detail.current_review + 1} 次审稿的第 1 轮</b>。</li>
          <li>之前驳回过的问题<b>不再带入</b>，金鱼和整理员都当第一次看。</li>
          <li>第 {detail.current_review} 次审稿的 {last?.rounds ?? 0} 轮原样保留，随时可以切回去看。</li>
          <li>新的第 1 轮会跟第 {detail.current_review} 次审稿的最后一轮比 diff，看得出定稿后改了什么。</li>
        </ul>
        <div class="dialog-foot">
          <button type="button" class="secondary" onclick={() => (reviewConfirm = false)}>取消</button>
          <button type="button" class="secondary strong" disabled={!!running || blocked} onclick={startReview}>开始第 {detail.current_review + 1} 次审稿</button>
        </div>
      </div>
    </div>
  {/if}

  {#if message}
    <div class="toast" class:error={message.error} role="status">{message.text}</div>
  {/if}

  <div class="body">
    <aside class="left">
      <div class="left-head">
        <span class="label">HD 的项目计划</span>
        <button type="button" class="icon-btn" onclick={refresh} title="刷新列表" aria-label="刷新列表">↻</button>
      </div>
      <div class="search">
        <input type="search" placeholder="搜索 PRD" aria-label="搜索 PRD" bind:value={query} />
      </div>
      {#if prds?.offline}
        <p class="offline">Notion 连不上，只显示本地已有的 PRD。</p>
      {/if}
      <div class="prd-list">
        {#if booting}
          <div class="group-head static"><span class="caret open" aria-hidden="true">›</span><span>进行中</span></div>
          <p class="loading loading-line" role="status">{@render dots()}正在从 Notion 读取…</p>
        {:else}
          {#each GROUPS as g (g.key)}
            {@const items = grouped[g.key]}
            {@const open = openGroups[g.key] || query.trim() !== "" || items.some((p) => p.page_id === selectedId)}
            {#if items.length || g.key === "active"}
              <button type="button" class="group-head" aria-expanded={open}
                onclick={() => (openGroups[g.key] = !open)}>
                <span class="caret" class:open aria-hidden="true">›</span>
                <span>{g.label}</span>
                <span class="count">{items.length}</span>
              </button>
              {#if open}
                {#each items as p (p.page_id)}
                  <button type="button" class="prd" class:on={p.page_id === selectedId} class:archived={g.key !== "active"}
                    title={p.title} onclick={() => select(p.page_id)}>
                    <span class="prd-title ellipsis">{p.name}</span>
                    {#if p.status === "go"}
                      <span class="pill go">GO</span>
                    {:else if p.rounds}
                      <span class="pill review">{p.reviews > 1 ? `第 ${p.reviews} 次 · ` : ""}第 {p.rounds} 轮</span>
                    {/if}
                  </button>
                {:else}
                  <p class="none">{query.trim() ? "没有匹配的 PRD" : "没有进行中的 PRD"}</p>
                {/each}
              {/if}
            {/if}
          {/each}
        {/if}
      </div>
      <div class="rules">
        <div class="label">规则页（Notion）</div>
        {#each rules as r}
          <button type="button" class="rule" disabled={!r.page_id} onclick={() => r.page_id && api.openNotion(r.page_id)}>
            <span>{r.name}</span><span class="muted">{r.page_id ? r.who : "未建"}</span>
          </button>
        {/each}
      </div>
    </aside>

    <main class="center">
      {#if booting}
        <section class="empty">
          <div class="loading" role="status">{@render dots()}正在读取 PRD 列表</div>
          <p class="muted small-text">Notion 慢的时候要几秒；连不上会改用本地已有的数据</p>
        </section>
      {:else if running === selectedId}
        <section class="running">
          <div class="muted small-text">{liveLabel}</div>
          <h1 class="running-title">{selectedItem?.title}</h1>
          <div class="live">
            <div class="live-head">
              <span class="strong">{liveHead}</span>
              <span class="muted live-clock">已用时 {clock(now - runStart)}</span>
            </div>
            <div class="steps">
              {#if live.pull}
                {@render liveRow("从 Notion 拉取快照", live.pull, false, "",
                  pullWait ? `PRD 刚改过，等 Notion 记下这次修改，最多 ${pullWait} 秒` : "")}
              {/if}
              {#if live.triage?.status === "queued"}
                <div class="step"><div class="step-row"><span class="step-mark"></span><span class="step-name">{FISH_KEYS.length} 条金鱼一起审稿</span></div></div>
                {#each liveFish as f (f.key)}
                  {@render liveRow(f.name, f.st, true, "")}
                {/each}
              {:else}
                {@render liveRow(`${FISH_KEYS.length} 条金鱼一起审稿`, { status: "done", start: null, end: null, error: null }, false, fishSpan)}
              {/if}
              {@render liveRow("整理员归类", live.triage, false, "", "把四条金鱼的发现合并、去重，给每条写建议")}
            </div>
            <details class="live-log">
              <summary>详细日志</summary>
              <ol class="log">
                {#each progressLines as line}<li>{line}</li>{/each}
              </ol>
            </details>
          </div>
          <p class="live-foot">通常 2～3 分钟。可以先去做别的事；中途关掉 app 也没关系，下次打开能接着跑没完成的部分。</p>
        </section>
      {:else if fresh && selectedItem}
        {@const o = outline?.id === selectedId ? outline : null}
        <section class="fresh">
          <div class="fresh-inner">
            <div>
              <div class="muted small-text">还没审过 ·
                <button type="button" class="link-btn small-text" onclick={() => selectedItem && api.openNotion(selectedItem.page_id)}>在 Notion 打开</button>
              </div>
              <h1 class="fresh-title">{selectedItem.title}</h1>
            </div>

            <div class="prd-card">
              <div class="label">这份 PRD</div>
              {#if !o || (!o.data && !o.error)}
                <p class="loading" role="status">{@render dots()}正在从 Notion 读取章节…</p>
              {:else if o.error}
                <p class="muted small-text">读不到 Notion，开跑时会再检查一次。（{o.error}）</p>
              {:else if o.data}
                {#if o.data.sections.length}
                  <ul class="sections">
                    {#each o.data.sections as h, i (i)}<li>{h}</li>{/each}
                  </ul>
                {:else}
                  <p class="muted small-text">没找到章节标题。</p>
                {/if}
                {#if o.data.key_problem}
                  {@const kp = KEY_PROBLEM[o.data.key_problem]}
                  {@const problem = o.data.key_problem}
                  <div class="key-row bad">
                    <svg class="key-mark" width="12" height="12" viewBox="0 0 12 12" aria-hidden="true"><path d="M3 3l6 6M9 3l-6 6" fill="none" stroke="#b3362b" stroke-width="1.6" stroke-linecap="round" /></svg>
                    <span class="key-bad">{kp.text}</span>
                    <span class="muted key-hint">{kp.hint}</span>
                    <button type="button" class="secondary small-btn key-copy" onclick={() => copyKeyPrompt(problem)}>复制给 Claude 的话</button>
                  </div>
                {:else}
                  <div class="key-row">
                    <svg class="key-mark" width="12" height="12" viewBox="0 0 12 12" aria-hidden="true"><path d="M2.5 6.2l2.3 2.3 4.7-5" fill="none" stroke="#1f7a4d" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" /></svg>
                    <span class="strong">{o.data.key_heading}</span>
                    <span class="muted key-hint">{o.data.key_items} 条 · 金鱼看不到，整理员拿它算命中率</span>
                  </div>
                {/if}
              {/if}
            </div>

            <div class="env-line">
              {#if !status}
                <span>{@render dots()}正在检查环境…</span>
              {:else if blocked}
                <span class="bad-text">环境有问题，见顶部的红色提示</span>
              {:else}
                <svg width="12" height="12" viewBox="0 0 12 12" aria-hidden="true"><path d="M2.5 6.2l2.3 2.3 4.7-5" fill="none" stroke="#1f7a4d" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" /></svg>
                <span>Notion、codex {status.codex_version ?? ""}、claude {status.claude_version ?? ""} 都正常</span>
              {/if}
              <button type="button" class="link-btn env-recheck" disabled={checking} onclick={recheckFresh}>{checking ? "检查中…" : "重新检查"}</button>
            </div>

            <div class="fresh-foot">
              <span class="flow">拉取快照 → 4 条金鱼审稿 → 整理员归类 → 你拍板<span class="faint">&nbsp;· 约 2～3 分钟，用订阅额度</span></span>
              <span class="fresh-go">
                {#if keyBad}<span class="blocked-hint">先处理清单</span>{:else if blocked}<span class="blocked-hint">先解决上面的问题</span>{/if}
                <button type="button" class="secondary small-btn" disabled={!!running || blocked || keyBad} onclick={() => run("start")}>
                  开始第 1 轮 →
                </button>
              </span>
            </div>
          </div>
        </section>
      {:else if !detail || detail.rounds.length === 0}
        <section class="empty">
          <div class="muted">还没审过</div>
          <h1>{selectedItem?.title ?? "选一份 PRD"}</h1>
        </section>
      {:else}
        <div class="center-head">
          <div>
            <div class="muted small-text">审稿中的 PRD</div>
            <h1>{detail.title}</h1>
          </div>
          {#if isGo}
            <div class="go-line">
              <span class="go-text">第 {detail.rounds.at(-1)?.n} 轮已 GO</span>
              <span class="faint" aria-hidden="true">·</span>
              <button type="button" class="link-btn" title="从第 1 轮重新审，之前的驳回不再带入"
                disabled={!!running || blocked} onclick={() => (reviewConfirm = true)}>重新审一遍</button>
            </div>
          {:else if viewingCurrent && !readyForNext}
            <div class="start">
              <button type="button" class="secondary" title="先从 Notion 拉取最新版本，再开始审稿"
                disabled={!!running || blocked} onclick={() => run("start")}>
                <span aria-hidden="true">＋</span>开始第 {nextRound} 轮
              </button>
              {#if blocked}<span class="blocked-hint">先解决上面的问题</span>{/if}
            </div>
          {/if}
        </div>

        <div class="tabs-row">
          {#if detail.reviews.length > 1}
            <div class="review-pick">
              <button type="button" class="review-btn" aria-haspopup="menu" aria-expanded={reviewMenu}
                onclick={() => (reviewMenu = !reviewMenu)}>
                第 {detail.review} 次审稿<span class="muted" aria-hidden="true">▾</span>
              </button>
              {#if reviewMenu}
                <div class="review-menu" role="menu">
                  {#each [...detail.reviews].reverse() as rv (rv.n)}
                    <button type="button" role="menuitem" class="review-item" class:on={rv.n === detail.review} onclick={() => pickReview(rv.n)}>
                      <span class="review-name">第 {rv.n} 次审稿</span>
                      <span class="muted small-text">{rv.rounds} 轮{rv.n === detail.current_review && rv.verdict !== "GO" ? " · 进行中" : ""}</span>
                      {#if rv.verdict === "GO"}<span class="pill go">GO</span>{/if}
                    </button>
                  {/each}
                </div>
              {/if}
            </div>
          {/if}
          <div class="tabs" role="tablist" aria-label="轮次">
            {#each detail.rounds as r, i (r.n)}
              <button type="button" role="tab" aria-selected={i === roundIdx} class="tab" class:on={i === roundIdx}
                onclick={() => { roundIdx = i; issueId = null; src = "all"; }}>
                <span class="tab-top">第 {r.n} 轮
                  {#if r.verdicts}<span class="verdict" style={verdictStyle(r.verdicts.prd)}>{r.verdicts.prd}</span>{/if}
                </span>
                <span class="muted small-text ellipsis tab-sub">{roundSummary(r)}</span>
              </button>
            {/each}
          </div>
        </div>

        {#if readyForNext}
          <div class="next-strip">
            <span class="muted">第 {nextRound - 1} 轮已生成交接文档。PRD 按它改好之后，就可以审下一轮。</span>
            {#if blocked}<span class="blocked-hint">先解决上面的问题</span>{/if}
            <button type="button" class="secondary small-btn" title="先从 Notion 拉取最新版本，再开始审稿"
              disabled={!!running || blocked} onclick={() => run("start")}>开始第 {nextRound} 轮</button>
          </div>
        {/if}

        {#if round && !triage}
          <section class="notice">
            {#if unfinished}
              {@const steps = stepsOf(round)}
              <div class="strong">这一轮没跑完</div>
              <p class="muted small-text">{unfinishedSummary(steps)}</p>
              <div class="steps">
                {#each steps as st (st.name)}
                  <div class="step">
                    <div class="step-row">
                      <span class="step-mark {st.status}" aria-hidden="true">
                        {#if st.status === "done"}
                          <svg width="12" height="12" viewBox="0 0 12 12"><path d="M2.5 6.2l2.3 2.3 4.7-5" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" /></svg>
                        {:else}<span class="dot"></span>{/if}
                      </span>
                      <span class="step-name">{st.name}</span>
                      <span class="step-status {st.status}">{STEP_STATE[st.status]}</span>
                    </div>
                    {#if st.error}<div class="step-error mono">{st.error}</div>{/if}
                  </div>
                {/each}
              </div>
              <div class="resume">
                <button type="button" class="secondary" disabled={!!running || blocked} onclick={() => run("resume")}>继续跑没完成的部分</button>
                {#if blocked}<span class="blocked-hint">先解决上面的问题</span>{:else}<span class="muted small-text">已完成的不会重跑</span>{/if}
              </div>
            {:else}
              <p class="muted">这一轮只拉了快照，没有审稿结果。</p>
            {/if}
          </section>
        {:else if round && triage}
          <div class="panes">
            <section class="card list">
              <div class="chips">
                <button type="button" class="chip" class:on={src === "all"} onclick={() => { src = "all"; issueId = null; }}>
                  全部 {triage.issues.length}
                </button>
                <span class="chip-sep" aria-hidden="true"></span>
                <span class="muted small-text">谁发现的</span>
                {#each ROLES as k}
                  {@const n = triage.issues.filter((i) => foundBy(i).includes(k)).length}
                  <button type="button" class="chip" class:on={src === k}
                    title="{ROLE_HINT[k]}。一条问题可能被几条金鱼同时发现，所以这几个数加起来会比全部多"
                    onclick={() => { src = k; issueId = null; }}>
                    {ROLE[k].label} {n}
                  </button>
                {/each}
              </div>
              <div class="issues">
                {#each shown as it (it.id)}
                  {@const st = stateOf(it)}
                  <button type="button" class="issue" class:on={cur?.id === it.id} onclick={() => (issueId = it.id)}>
                    <span class="issue-meta">
                      <span class="sev" style="color:{SEV[it.severity].color}" title="严重度">{SEV[it.severity].label}</span>
                      {#each foundBy(it) as r}
                        <span class="role" style="color:{ROLE[r].color};background:{ROLE[r].bg}" title={ROLE_HINT[r]}>{ROLE[r].label}</span>
                      {/each}
                      <span class="state" style="color:{st.color}">{st.text}</span>
                    </span>
                    <span class="issue-title">{it.title}</span>
                    <span class="issue-sections muted">{sectionsOf(it)}</span>
                  </button>
                {:else}
                  <p class="muted center-text">这一类没有问题。</p>
                {/each}
              </div>
              {#if round.latest}
                <div class="list-foot">
                  {#if undecided.length}
                    <span class="muted foot-text">还有 <b class="pending">{undecided.length}</b> 条待决定，拍完才能生成交接文档</span>
                  {:else if !round.has_handoff}
                    <span class="muted foot-text">都拍完了</span>
                    <button type="button" class="secondary" onclick={handoff}>生成交接文档</button>
                  {:else if round.handoff_stale}
                    <span class="foot-text stale">生成后你改过决定，交接文档已过时</span>
                    <button type="button" class="secondary small-btn" onclick={handoff}>重新生成</button>
                  {:else}
                    <span class="foot-text done">
                      <svg width="12" height="12" viewBox="0 0 12 12" aria-hidden="true"><path d="M2.5 6.2l2.3 2.3 4.7-5" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" /></svg>
                      交接文档已生成
                    </span>
                    <button type="button" class="secondary small-btn" onclick={copyHandoff}>复制</button>
                  {/if}
                </div>
              {/if}
            </section>

            <section class="card detail">
              {#if cur}
                {@const d = decisionOf(cur.id)}
                <div class="detail-body">
                  <div class="issue-meta">
                    <span class="sev" style="color:{SEV[cur.severity].color}" title="严重度">{SEV[cur.severity].label}</span>
                    {#each foundBy(cur) as r}
                      <span class="role" style="color:{ROLE[r].color};background:{ROLE[r].bg}" title={ROLE_HINT[r]}>{ROLE[r].label}</span>
                    {/each}
                  </div>
                  <h2>{cur.title}</h2>
                  <p class="issue-sections muted">{sectionsOf(cur)}</p>
                  <div class="field"><div class="field-label">问题</div><p>{cur.problem}</p></div>
                  <div class="field"><div class="field-label">影响</div><p>{cur.impact}</p></div>
                  <div class="advice">
                    <div class="advice-head">
                      <span>整理员</span>
                      <span class="advice-pill" class:reject={cur.advice === "reject"}>建议{word(cur.advice)}</span>
                    </div>
                    <p>{cur.reason}</p>
                    {#if cur.suggested_change}
                      <div class="change"><div class="field-label">建议改法</div><p>{cur.suggested_change}</p></div>
                    {/if}
                  </div>
                  {#if cur.rejected_as}
                    <p class="auto">这条和历史驳回的 {cur.rejected_as} 是同一个问题，已自动驳回，不用再拍板；判断错了可以改成接受。</p>
                  {/if}
                  {#if cur.refs.length}
                    <details class="refs">
                      <summary>金鱼原话（{cur.refs.length} 条）</summary>
                      {#each cur.refs as ref}
                        {@const who = ref.split("#")[0]}
                        <div class="ref"><span class="ref-who">{FISH_NAMES[who] ?? who}金鱼</span><p>{resolveRef(round.fish, ref) ?? "（找不到原文）"}</p></div>
                      {/each}
                    </details>
                  {/if}
                  {#if cur.section.trim()}
                    {#if !hasNext}
                      <div class="sec-wait">这一节改了什么，跑完第 {round.n + 1} 轮后显示在这里。</div>
                    {:else if issueDiff?.key === issueKey && issueDiff.data}
                      {@const idf = issueDiff.data}
                      {@const there = roundLabel(idf.next)}
                      <div class="sec-diffs">
                        {#each idf.sections as sd (sd.reference)}
                          {#if sd.status === "changed"}
                            <div class="sec-diff">
                              <div class="sec-diff-head">
                                <span class="strong">这一节在{there}的改动</span>
                                <span class="muted small-text">{sd.reference} · {changeCount(sd.added, sd.removed)}</span>
                              </div>
                              <div class="sec-diff-body">{@render diffLines(sd.lines)}</div>
                            </div>
                          {:else}
                            <p class="sec-note muted small-text">
                              {sd.reference}：{sd.status === "unchanged" ? `${there}没有改动` : sd.status === "ambiguous" ? `${there}里有不止一个这样的标题，分不出是哪一节` : `${there}里找不到这个标题`}
                            </p>
                          {/if}
                        {/each}
                      </div>
                    {:else if issueDiff?.key === issueKey && issueDiff.error}
                      <p class="sec-note bad-text small-text">读不到下一轮的快照：{issueDiff.error}</p>
                    {/if}
                  {/if}
                </div>

                <div class="decide-bar">
                  {#if !round.latest}
                    <div class="decide-row">
                      <span class="decided">{d ? `你${word(d)}了` : stateOf(cur).text}</span>
                      <span class="muted small-text push">历史轮次只读</span>
                    </div>
                    {#if note}<p class="note-ro">备注：{note}</p>{/if}
                  {:else}
                    {#if d}
                      <div class="decide-row">
                        <span class="decided" class:accept={d === "accept"}>
                          <svg width="12" height="12" viewBox="0 0 12 12" aria-hidden="true"><path d="M2.5 6.2l2.3 2.3 4.7-5" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" /></svg>
                          你{word(d)}了
                        </span>
                        <span class="small-text" class:muted={d === cur.advice} class:against={d !== cur.advice}>
                          {d === cur.advice ? "跟整理员建议一致" : "跟整理员建议相反"}
                        </span>
                        <button type="button" class="link push" onclick={() => decide(d === "accept" ? "reject" : "accept")}>
                          改成{word(d === "accept" ? "reject" : "accept")}
                        </button>
                      </div>
                    {:else if cur.rejected_as}
                      <div class="decide-row">
                        <span class="decided">已自动驳回 · 同 {cur.rejected_as}</span>
                        <button type="button" class="link push" onclick={() => decide("accept")}>改成接受</button>
                      </div>
                    {:else}
                      <div class="decide-row">
                        <span class="muted small-text">整理员建议<b class="adv" class:reject={cur.advice === "reject"}>{word(cur.advice)}</b></span>
                        <span class="decide-btns push">
                          {#each CHOICES as a}
                            <button type="button" class="choice {a}" class:advised={cur.advice === a} onclick={() => decide(a)}>
                              {cur.advice === a ? `同意，${word(a)}` : word(a)}
                            </button>
                          {/each}
                        </span>
                      </div>
                    {/if}
                    {#if showNote}
                      <input class="note" type="text" aria-label="备注" bind:this={noteEl} bind:value={note} onblur={saveNote}
                        onkeydown={(e) => e.key === "Enter" && e.currentTarget.blur()}
                        placeholder={d === "reject" ? "为什么驳回？（可选，会写进交接文档）" : "备注（可选，会写进交接文档）"} />
                    {:else}
                      <button type="button" class="link add-note" onclick={openNote}>＋ 加备注</button>
                    {/if}
                  {/if}
                </div>
              {:else}
                <p class="muted center-text detail-empty">选一条问题。</p>
              {/if}
            </section>
          </div>
        {/if}
      {/if}
    </main>

    <aside class="right">
      {#if round?.triage}
        {@const t = round.triage}
        {@const prd = round.verdicts?.prd}
        {@const rate = hitRate(t)}
        {@const counted = detail?.rounds.filter((r) => r.triage) ?? []}
        {@const at = counted.findIndex((r) => r.n === round.n)}
        {@const diff = at > 0 ? t.issues.length - counted[at - 1].triage!.issues.length : null}
        {@const fails = t.spec_checks.filter((c) => !c.pass)}
        {@const passes = t.spec_checks.filter((c) => c.pass)}
        <section>
          <h2 class="label">第 {round.n} 轮结论</h2>
          <div class="line prd-line">
            <div class="line-top">
              <span class="strong">整份 PRD</span>
              <span class="verdict" style={verdictStyle(prd)}>{prd ?? "—"}</span>
            </div>
            {#if verdictNote}<div class="line-why muted">{verdictNote}</div>{/if}
          </div>
          {#each lines as l (l.role)}
            <div class="line">
              <div class="line-top">
                <span>{ROLE[l.role].label}金鱼</span>
                <span class="verdict" style={verdictStyle(l.verdict)}>{l.verdict ?? "—"}</span>
              </div>
              {#if l.verdict !== "GO" && l.reasons.length}<div class="line-why muted">{l.reasons.join(" · ")}</div>{/if}
            </div>
          {/each}
          {#each round.warnings as w}<p class="warn">{w}</p>{/each}
        </section>
        <section>
          <h2 class="label">关键决策命中率</h2>
          <div class="hit">
            <span class="hit-num" class:ok={rate >= HIT_THRESHOLD}>{pct(rate)}</span>
            <span class="muted small-text">{t.key_decision_hits} / {t.key_decision_total} 条 · 要 {pct(HIT_THRESHOLD)}</span>
          </div>
          <div class="meter" title="部分说对记 0.5 条">
            <span class="meter-fill" class:ok={rate >= HIT_THRESHOLD} style="width:{Math.min(100, rate * 100)}%"></span>
            <span class="meter-mark" style="left:{HIT_THRESHOLD * 100}%" aria-hidden="true"></span>
          </div>
          {#if t.key_decisions.some((k) => k.result !== "hit")}
            {@const missed = t.key_decisions.filter((k) => k.result !== "hit")}
            <div class="sub-label">复述金鱼没说对的 {missed.length} 条</div>
            {#each missed as k}
              <p class="kd"><span class="kd-tag" class:miss={k.result === "miss"}>{k.result === "miss" ? "漏了" : "说对一半"}</span>{k.item}</p>
            {/each}
          {/if}
        </section>
        <section>
          <h2 class="label">每轮问题数{diff === null ? "" : diff < 0 ? ` · 比上一轮少 ${-diff} 条` : diff > 0 ? ` · 比上一轮多 ${diff} 条` : " · 跟上一轮一样"}</h2>
          {#each counted as r (r.n)}
            {@const c = r.triage!.issues.length}
            <div class="hbar" class:on={r.n === round.n}>
              <span class="muted small-text">第 {r.n} 轮</span>
              <span class="hbar-track"><span class="hbar-fill" style="width:{(c / maxIssues) * 100}%"></span></span>
              <span class="hbar-num">{c} 条</span>
            </div>
          {/each}
        </section>
        <section>
          <h2 class="label">规范达标 · {passes.length} / {t.spec_checks.length} <span class="label-note">· 不影响 GO</span></h2>
          {#each fails as c}
            {@const why = specNote(round.fish, c.item)}
            <div class="spec-fail">
              <div class="spec-line"><span class="spec-x" aria-hidden="true">✕</span><span>{@html inlineMd(c.item)}</span></div>
              {#if why}<p class="spec-note muted">{why}</p>{/if}
            </div>
          {/each}
          {#if passes.length}
            <details class="spec-pass">
              <summary>通过的 {passes.length} 条</summary>
              {#each passes as c}
                <div class="spec-line"><span class="spec-ok" aria-hidden="true">✓</span><span>{@html inlineMd(c.item)}</span></div>
              {/each}
            </details>
          {/if}
        </section>
      {:else if !booting}
        <p class="muted small-text">跑完一轮后，这里显示三条金鱼的结论、命中率和规范达标。</p>
      {/if}
    </aside>
  </div>
</div>

<style>
  /* The window itself never scrolls or rubber-bands; only the panes inside do. */
  :global(html), :global(body) { height: 100%; overflow: hidden; overscroll-behavior: none; }
  :global(body) {
    margin: 0;
    background: #f4f3ef;
    color: #1f2328;
    font-family: -apple-system, BlinkMacSystemFont, "PingFang SC", "Helvetica Neue", sans-serif;
    font-size: 13px;
  }
  :global(button) { font: inherit; }
  .app { height: 100vh; display: flex; flex-direction: column; overflow: hidden; }
  .top { flex: none; display: flex; justify-content: space-between; align-items: center; gap: 12px; padding: 12px 20px; background: #fff; border-bottom: 1px solid rgba(0,0,0,.08); }
  .brand { display: flex; align-items: baseline; gap: 12px; }
  .name { font-weight: 700; font-size: 16px; }
  .preflight { flex: none; display: flex; align-items: center; gap: 12px; padding: 10px 20px; background: #fbeceb; border-bottom: 1px solid #f0cfcb; color: #8f2a21; }
  .preflight-lines { flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 4px; }
  .preflight p { margin: 0; display: flex; align-items: baseline; gap: 10px; line-height: 1.6; }
  .preflight code { font-family: ui-monospace, "SF Mono", Menlo, monospace; font-size: 12px; background: #fff; padding: 1px 6px; margin: 0 4px; border-radius: 4px; user-select: all; -webkit-user-select: all; word-break: break-all; }
  .bad-dot { flex: none; width: 8px; height: 8px; border-radius: 50%; background: #b3362b; transform: translateY(-1px); }
  .recheck { flex: none; min-height: 30px; padding: 0 12px; background: #fff; border: 1px solid #e3b5b0; border-radius: 7px; color: #8f2a21; cursor: pointer; font-size: 12px; font-weight: 500; }
  .recheck:disabled { opacity: .6; cursor: default; }
  .secondary { min-height: 34px; display: inline-flex; align-items: center; gap: 6px; padding: 0 14px; background: #fff; color: #1f2328; border: 1px solid rgba(0,0,0,.18); border-radius: 8px; cursor: pointer; font-weight: 500; }
  .secondary:hover:not(:disabled) { background: #f7f6f2; }
  .secondary:disabled { color: #8a8f96; cursor: default; }
  .next-strip { flex: none; display: flex; align-items: center; gap: 12px; margin: 0 22px 12px; padding: 8px 12px 8px 14px; background: #fff; border: 1px solid rgba(0,0,0,.08); border-radius: 8px; }
  .next-strip > .muted { flex: 1; min-width: 0; }
  .small-btn { min-height: 30px; padding: 0 12px; border-radius: 7px; font-size: 12px; font-weight: 600; }
  .start { display: flex; flex-direction: column; align-items: flex-end; gap: 6px; }
  .blocked-hint { font-size: 11px; color: #8f2a21; }
  .muted { color: #5f6670; }
  .small-text { font-size: 11px; }
  .strong { font-weight: 600; }
  .mono { font-family: ui-monospace, "SF Mono", Menlo, monospace; font-size: 11px; }
  .center-text { text-align: center; padding: 24px; margin: 0; }
  .ellipsis { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .label { font-size: 11px; font-weight: 600; color: #5f6670; margin: 0 0 8px; }
  .toast { position: fixed; top: 56px; left: 50%; transform: translateX(-50%); z-index: 10; max-width: 640px; padding: 10px 16px; border-radius: 8px; background: #1f2328; color: #fff; box-shadow: 0 6px 20px rgba(0,0,0,.18); }
  .toast.error { background: #8f2a21; }
  .body { flex: 1; min-height: 0; display: flex; }

  .left { flex: 0 0 248px; display: flex; flex-direction: column; background: #fbfaf7; border-right: 1px solid rgba(0,0,0,.08); min-height: 0; }
  .left-head { display: flex; justify-content: space-between; align-items: center; padding: 14px 16px 8px; }
  .left-head .label { margin: 0; }
  .offline { margin: 0 16px 8px; font-size: 11px; color: #a86b00; }
  .icon-btn { width: 28px; height: 28px; display: grid; place-items: center; padding: 0; background: none; border: none; border-radius: 6px; color: #5f6670; cursor: pointer; font-size: 15px; }
  .icon-btn:hover { background: #ecebe5; color: #1f2328; }
  .search { padding: 0 12px 8px; }
  .search input { width: 100%; box-sizing: border-box; height: 30px; padding: 0 10px; border: 1px solid rgba(0,0,0,.1); border-radius: 7px; background: #fff; font: inherit; font-size: 12px; color: #1f2328; }
  .search input:focus { outline: none; border-color: #4a5a7a; }
  .prd-list { flex: 1; min-height: 0; overflow-y: auto; padding: 0 8px 8px; }
  .group-head { width: 100%; display: flex; align-items: center; gap: 6px; min-height: 28px; margin-top: 6px; padding: 0 8px; background: none; border: none; color: #5f6670; cursor: pointer; font-size: 11px; font-weight: 600; text-align: left; }
  .group-head:hover { color: #1f2328; }
  .caret { width: 10px; display: inline-block; transition: transform .12s; }
  .caret.open { transform: rotate(90deg); }
  .count { margin-left: auto; font-weight: 400; color: #8a8f96; }
  .prd { width: 100%; min-height: 34px; display: flex; align-items: center; gap: 8px; padding: 0 10px 0 24px; text-align: left; background: transparent; border: none; border-radius: 7px; color: #1f2328; cursor: pointer; }
  .prd:hover { background: #f1f0eb; }
  .prd.on { background: #e6e4dc; }
  .prd.on .prd-title { font-weight: 600; }
  .prd.archived { color: #5f6670; }
  .prd-title { flex: 1; }
  .pill { flex: none; font-size: 10px; font-weight: 600; line-height: 1; padding: 4px 7px; border-radius: 999px; }
  .pill.review { color: #b85414; background: #fbefe6; }
  .pill.go { color: #1f7a4d; background: #e4f2ea; }
  .none { margin: 2px 0 6px 24px; font-size: 11px; color: #8a8f96; }
  .group-head.static { cursor: default; }
  .group-head.static:hover { color: #5f6670; }
  .loading { display: flex; align-items: center; color: #5f6670; }
  .loading-line { margin: 6px 0 6px 24px; font-size: 11px; }
  .dots { display: inline-flex; gap: 3px; margin-right: 6px; }
  .dots span { width: 5px; height: 5px; border-radius: 50%; background: #5f6670; animation: blink 1.2s infinite both; }
  .dots span:nth-child(2) { animation-delay: .2s; }
  .dots span:nth-child(3) { animation-delay: .4s; }
  @keyframes blink { 0%, 80%, 100% { opacity: .25; } 40% { opacity: 1; } }
  @media (prefers-reduced-motion: reduce) { .dots span { animation: none; opacity: .6; } }
  .rules { flex: none; padding: 12px 16px 16px; border-top: 1px solid rgba(0,0,0,.08); }
  .rule { width: 100%; display: flex; justify-content: space-between; min-height: 26px; align-items: center; padding: 0; background: none; border: none; color: #4a5a7a; cursor: pointer; font-family: ui-monospace, "SF Mono", Menlo, monospace; font-size: 11px; }
  .rule:disabled { cursor: default; color: #8a8f96; }

  .center { flex: 1; min-width: 0; min-height: 0; display: flex; flex-direction: column; }
  .center-head { flex: none; display: flex; justify-content: space-between; align-items: center; gap: 12px; padding: 16px 22px 12px; }
  h1 { margin: 4px 0 0; font-size: 22px; font-weight: 600; }
  h2 { font-size: 16px; font-weight: 600; line-height: 1.5; margin: 10px 0 0; }
  .empty, .running { flex: 1; display: flex; flex-direction: column; align-items: center; justify-content: center; gap: 14px; padding: 40px; text-align: center; }
  .empty p { max-width: 420px; line-height: 1.6; margin: 0; }
  .steps { margin-top: 10px; }
  .step { padding: 7px 0; border-bottom: 1px solid rgba(0,0,0,.06); }
  .step-row { display: flex; align-items: center; gap: 10px; }
  .step-mark { width: 16px; display: inline-grid; place-items: center; }
  .step-mark.done { color: #1f7a4d; }
  .step-mark .dot { width: 8px; height: 8px; box-sizing: border-box; border-radius: 50%; border: 1.5px solid #b9b6ae; }
  .step-mark.failed .dot { background: #b3362b; border-color: #b3362b; }
  .step-mark.running .dot { background: #b85414; border-color: #b85414; }
  .step-name { flex: 1; }
  .step-status { color: #5f6670; }
  .step-status.done { color: #1f7a4d; }
  .step-status.failed { color: #b3362b; font-weight: 600; }
  .step-status.running { color: #b85414; font-weight: 600; }
  .step-error { margin: 2px 0 0 26px; color: #5f6670; white-space: pre-wrap; word-break: break-word; }
  .running-title { margin-top: 0; }
  .live { width: min(520px, 100%); box-sizing: border-box; padding: 14px 20px 16px; text-align: left; background: #fff; border: 1px solid rgba(0,0,0,.08); border-radius: 10px; }
  .live-head { display: flex; justify-content: space-between; align-items: baseline; gap: 12px; }
  .live-clock, .step-time { font-variant-numeric: tabular-nums; }
  .live-clock { font-size: 12px; }
  .step.indent { padding-left: 26px; }
  .step-time { flex: none; width: 40px; text-align: right; font-size: 11px; color: #8a8f96; }
  .step-status.queued { color: #8a8f96; }
  .spinner { width: 12px; height: 12px; box-sizing: border-box; border-radius: 50%; border: 1.6px solid #e3d3c4; border-top-color: #b85414; animation: spin .9s linear infinite; }
  @keyframes spin { to { transform: rotate(360deg); } }
  @media (prefers-reduced-motion: reduce) { .spinner { animation: none; } }
  .live-log { margin-top: 10px; }
  .live-log summary { cursor: pointer; font-size: 12px; font-weight: 600; color: #2b3240; }
  .live-log .log { margin-top: 8px; border: none; background: #fbfaf7; }
  .live-foot { margin: 0; max-width: 520px; font-size: 11px; line-height: 1.6; color: #8a8f96; }
  .resume { display: flex; align-items: center; gap: 12px; margin-top: 14px; }
  .log { width: 100%; box-sizing: border-box; max-height: 40vh; overflow-y: auto; text-align: left; margin: 0; padding: 12px 12px 12px 32px; background: #fff; border: 1px solid rgba(0,0,0,.08); border-radius: 8px; font-family: ui-monospace, "SF Mono", Menlo, monospace; font-size: 12px; line-height: 1.8; }

  .tabs-row { flex: none; display: flex; align-items: center; gap: 14px; padding: 0 22px 14px; }
  /* Scrolls sideways only when rounds do not fit, with no visible bar. */
  .tabs { flex: 1; min-width: 0; display: flex; overflow-x: auto; padding-right: 1px; scrollbar-width: none; }
  .tabs::-webkit-scrollbar { display: none; }
  .review-pick { flex: none; position: relative; }
  .review-btn { display: inline-flex; align-items: center; gap: 4px; padding: 4px 0; background: none; border: none; color: #1f2328; cursor: pointer; font: inherit; font-size: 12px; font-weight: 600; }
  .review-menu { position: absolute; top: calc(100% + 6px); left: 0; z-index: 15; width: 260px; padding: 6px; background: #fff; border: 1px solid rgba(0,0,0,.12); border-radius: 10px; box-shadow: 0 8px 24px rgba(0,0,0,.12); }
  .review-item { width: 100%; display: flex; align-items: center; gap: 8px; padding: 8px 10px; background: none; border: none; border-radius: 6px; color: #1f2328; cursor: pointer; font: inherit; text-align: left; }
  .review-item:hover { background: #f7f6f2; }
  .review-item.on { background: #f1f0ea; }
  .review-name { flex: 1; font-weight: 500; }
  .go-line { display: flex; align-items: center; gap: 6px; font-size: 12px; }
  .go-text { color: #1f7a4d; }
  .faint { color: #8a8f96; }
  .dl { display: flex; gap: 8px; min-height: 1.7em; padding: 0 12px; font-family: ui-monospace, "SF Mono", Menlo, monospace; font-size: 11.5px; line-height: 1.7; white-space: pre-wrap; word-break: break-word; color: #3a4049; }
  .dl-mark { flex: none; width: 10px; }
  .dl.add { background: #e6f2eb; color: #1d5e3c; }
  .dl.del { background: #fbe9e7; color: #8f2a21; }
  .dl.del .dl-text { text-decoration: line-through; text-decoration-color: rgba(143,42,33,.35); }
  .dl.gap { display: block; padding: 2px 12px; background: #faf9f6; color: #8a8f96; font-family: inherit; font-size: 11px; }
  .sec-wait { margin-top: 16px; padding: 10px 12px; border: 1px dashed rgba(0,0,0,.15); border-radius: 8px; font-size: 12px; color: #5f6670; }
  .sec-diffs { margin-top: 16px; display: flex; flex-direction: column; gap: 8px; }
  .sec-diff { border: 1px solid rgba(0,0,0,.08); border-radius: 8px; overflow: hidden; }
  .sec-diff-head { display: flex; justify-content: space-between; align-items: center; gap: 10px; padding: 7px 10px; background: #fbfaf7; border-bottom: 1px solid rgba(0,0,0,.08); font-size: 12px; }
  .sec-diff-body { padding: 4px 0; }
  .sec-diff-body .dl { font-size: 11px; padding: 0 10px; }
  .sec-note { margin: 0; line-height: 1.6; }
  .confirm-list { margin: 0; padding: 4px 20px 8px 38px; line-height: 1.8; color: #2b3240; }
  .tab { flex: 0 0 200px; min-height: 58px; display: flex; flex-direction: column; justify-content: center; gap: 4px; padding: 8px 14px; text-align: left; background: #f9f8f4; border: 1px solid rgba(0,0,0,.1); margin-right: -1px; color: #1f2328; cursor: pointer; }
  .tab.on { background: #fff; border-color: #1f2328; position: relative; z-index: 1; }
  .tab-sub { max-width: 100%; }
  .tab-top { display: flex; justify-content: space-between; align-items: center; gap: 8px; width: 100%; font-weight: 600; white-space: nowrap; }
  .tab-top .verdict { flex: none; white-space: nowrap; }
  .verdict { font-size: 11px; font-weight: 600; border-radius: 4px; padding: 2px 8px; }
  .notice { margin: 0 22px; max-width: 560px; padding: 16px 20px; background: #fff; border: 1px solid rgba(0,0,0,.08); border-radius: 10px; line-height: 1.7; }
  .notice > p { margin: 2px 0 0; }

  .panes { flex: 1; min-height: 0; display: flex; gap: 14px; padding: 0 22px 18px; }
  .card { min-width: 0; display: flex; flex-direction: column; background: #fff; border: 1px solid rgba(0,0,0,.08); border-radius: 10px; min-height: 0; }
  .list { flex: 1 1 0; }
  .detail { flex: 1.15 1 0; }
  .detail-body { flex: 1; min-height: 0; overflow-y: auto; padding: 18px 20px; }
  .detail-empty { padding: 18px 20px; }
  .chips { flex: none; display: flex; flex-wrap: wrap; gap: 6px; padding: 12px 14px; border-bottom: 1px solid rgba(0,0,0,.06); }
  .chip { min-height: 30px; padding: 0 10px; border-radius: 999px; border: 1px solid rgba(0,0,0,.12); background: #fff; color: #2b3240; cursor: pointer; font-size: 12px; }
  .chip.on { background: #1f2328; color: #fff; border-color: #1f2328; }
  .issues { flex: 1; min-height: 0; overflow-y: auto; padding: 8px; }
  .issue { width: 100%; display: flex; flex-direction: column; gap: 6px; padding: 12px; margin-bottom: 6px; text-align: left; background: #fff; border: 1px solid rgba(0,0,0,.08); border-radius: 8px; color: #1f2328; cursor: pointer; }
  .issue.on { background: #f4f5f8; border-color: #4a5a7a; }
  .issue-meta { display: flex; flex-wrap: wrap; align-items: center; gap: 5px; width: 100%; font-size: 11px; }
  .issue-meta .sev { margin-right: 4px; }
  .issue-sections { font-size: 11px; line-height: 1.5; }
  .chips { align-items: center; }
  .chip-sep { width: 1px; height: 18px; margin: 0 4px; background: rgba(0,0,0,.12); }
  .role { font-weight: 600; border-radius: 4px; padding: 2px 7px; }
  .sev { font-weight: 600; }
  .state { margin-left: auto; font-weight: 600; }
  .issue-title { font-size: 13px; line-height: 1.5; font-weight: 500; }
  .list-foot { flex: none; display: flex; justify-content: space-between; align-items: center; gap: 8px; min-height: 56px; box-sizing: border-box; padding: 10px 14px; border-top: 1px solid rgba(0,0,0,.06); }
  .foot-text { font-size: 12px; }
  .foot-text .pending { color: #b85414; }
  .foot-text.done { display: inline-flex; align-items: center; gap: 5px; color: #1f7a4d; font-weight: 600; }
  .foot-text.stale { color: #b85414; }

  .detail h2 { margin-top: 8px; }
  .detail .issue-sections { margin: 4px 0 0; }
  .field { margin-top: 14px; }
  .field-label { font-size: 11px; font-weight: 600; color: #5f6670; }
  .field p, .change p { margin: 4px 0 0; line-height: 1.65; }
  .advice { margin-top: 16px; padding: 12px 14px; background: #f4f5f8; border-radius: 8px; }
  .advice-head { display: flex; align-items: center; gap: 8px; font-size: 11px; font-weight: 600; color: #4a5a7a; }
  .advice-pill { color: #1f7a4d; background: #e6f2eb; border-radius: 999px; padding: 2px 9px; }
  .advice-pill.reject { color: #5f6670; background: #e9e7e1; }
  .advice p { margin: 6px 0 0; line-height: 1.6; color: #2b3240; }
  .change { margin-top: 10px; padding-left: 10px; border-left: 2px solid #c9cfdb; }
  .auto { margin: 12px 0 0; padding: 10px 12px; background: #fbfaf7; border-radius: 8px; color: #5f6670; line-height: 1.6; }
  .refs { margin-top: 18px; }
  .refs summary { cursor: pointer; font-size: 12px; font-weight: 600; }
  .ref { margin-top: 8px; padding: 8px 10px; background: #fbfaf7; border-radius: 6px; }
  .ref-who { font-size: 11px; font-weight: 600; color: #5f6670; }
  .ref p { margin: 4px 0 0; line-height: 1.6; }

  .decide-bar { flex: none; display: flex; flex-direction: column; gap: 8px; padding: 12px 20px 14px; border-top: 1px solid rgba(0,0,0,.08); background: #fbfaf7; border-radius: 0 0 10px 10px; }
  .decide-row { display: flex; flex-wrap: wrap; align-items: center; gap: 8px; min-height: 34px; }
  .push { margin-left: auto; }
  .decide-btns { display: inline-flex; gap: 6px; }
  .choice { min-height: 34px; padding: 0 14px; border-radius: 8px; border: 1px solid rgba(0,0,0,.14); background: #fff; color: #5f6670; cursor: pointer; font-weight: 500; }
  .choice:hover { border-color: rgba(0,0,0,.3); }
  .choice.advised.accept { border-color: #1f7a4d; background: #e6f2eb; color: #1f7a4d; }
  .choice.advised.reject { border-color: #5f6670; background: #e9e7e1; color: #1f2328; }
  .adv { color: #1f7a4d; }
  .adv.reject { color: #1f2328; }
  .decided { display: inline-flex; align-items: center; gap: 5px; font-weight: 600; color: #1f2328; }
  .decided.accept { color: #1f7a4d; }
  .against { color: #b85414; }
  .link { background: none; border: none; padding: 0; font: inherit; font-size: 12px; color: #5f6670; cursor: pointer; text-decoration: underline; }
  .link:hover { color: #1f2328; }
  .add-note { align-self: flex-start; text-decoration: none; }
  .note { width: 100%; box-sizing: border-box; min-height: 34px; padding: 0 10px; border: 1px solid rgba(0,0,0,.14); border-radius: 8px; background: #fff; font: inherit; }
  .note-ro { margin: 0; font-size: 12px; color: #5f6670; }

  .right { flex: 0 0 272px; min-height: 0; overflow-y: auto; background: #fbfaf7; border-left: 1px solid rgba(0,0,0,.08); padding: 16px; display: flex; flex-direction: column; gap: 18px; }
  .right section { display: flex; flex-direction: column; }
  .line { padding: 8px 0; border-bottom: 1px solid rgba(0,0,0,.06); }
  .prd-line { padding-top: 0; border-bottom-color: rgba(0,0,0,.12); }
  .line-top { display: flex; justify-content: space-between; align-items: center; }
  .line-why { margin-top: 2px; font-size: 11px; line-height: 1.5; }
  .warn { margin: 6px 0 0; font-size: 11px; color: #a86b00; }
  .hit { display: flex; align-items: baseline; gap: 8px; }
  .hit-num { font-size: 28px; font-weight: 600; color: #b3362b; }
  .hit-num.ok { color: #1f7a4d; }
  .meter { position: relative; height: 6px; margin-top: 8px; background: #ecebe5; border-radius: 3px; }
  .meter-fill { display: block; height: 100%; background: #b3362b; border-radius: 3px; }
  .meter-fill.ok { background: #1f7a4d; }
  .meter-mark { position: absolute; top: -3px; width: 2px; height: 12px; margin-left: -1px; background: #1f2328; }
  .sub-label { margin-top: 12px; font-size: 11px; font-weight: 600; color: #5f6670; }
  .kd { margin: 6px 0 0; font-size: 12px; line-height: 1.5; color: #2b3240; }
  .kd-tag { margin-right: 4px; font-size: 11px; font-weight: 600; border-radius: 4px; padding: 1px 6px; color: #a86b00; background: #fbf1dc; }
  .kd-tag.miss { color: #b3362b; background: #fbeae8; }
  .hbar { display: flex; align-items: center; gap: 8px; min-height: 22px; }
  .hbar > .muted { flex: none; width: 44px; }
  .hbar-track { flex: 1; height: 8px; }
  .hbar-fill { display: block; height: 100%; background: #d9d6cd; border-radius: 4px; }
  .hbar.on .hbar-fill { background: #b85414; }
  .hbar-num { flex: none; width: 40px; text-align: right; font-size: 12px; }
  .hbar.on .hbar-num { font-weight: 600; }
  .label-note { font-weight: 400; }
  .spec-fail { padding: 7px 0; border-bottom: 1px solid rgba(0,0,0,.06); }
  .spec-line { display: flex; gap: 8px; font-size: 12px; line-height: 1.5; }
  .spec-line :global(code) { font-family: ui-monospace, "SF Mono", Menlo, monospace; font-size: 11px; background: #ecebe5; padding: 0 4px; border-radius: 3px; }
  .spec-line :global(b) { font-weight: 600; }
  .spec-x { flex: none; width: 10px; font-weight: 700; color: #b3362b; }
  .spec-ok { flex: none; width: 10px; font-weight: 700; color: #1f7a4d; }
  .spec-note { margin: 2px 0 0 18px; font-size: 11px; line-height: 1.5; }
  .spec-pass { margin-top: 8px; }
  .spec-pass summary { cursor: pointer; font-size: 12px; font-weight: 600; color: #2b3240; }
  .spec-pass .spec-line { margin-top: 6px; }
  .backdrop { position: fixed; inset: 0; z-index: 20; display: grid; place-items: center; padding: 16px; background: rgba(31,35,40,.28); }
  .dialog { width: min(540px, 100%); max-height: calc(100vh - 32px); overflow-y: auto; background: #fff; border-radius: 12px; box-shadow: 0 18px 50px rgba(0,0,0,.22); }
  .dialog-head { display: flex; justify-content: space-between; align-items: center; padding: 14px 16px 10px 20px; }
  .dialog-head h2 { margin: 0; font-size: 15px; font-weight: 600; }
  .dialog-sec { padding: 14px 20px 8px; border-top: 1px solid rgba(0,0,0,.08); }
  .label-note { font-weight: 400; color: #8a8f96; margin-left: 6px; }
  .field { display: grid; grid-template-columns: 110px 1fr; align-items: start; gap: 12px; padding: 6px 0; }
  .field > label, .field-label { padding-top: 7px; color: #2b3240; }
  .text-in { width: 100%; height: 32px; box-sizing: border-box; padding: 0 10px; border: 1px solid rgba(0,0,0,.14); border-radius: 7px; background: #fff; color: #1f2328; font-family: ui-monospace, "SF Mono", Menlo, monospace; font-size: 12px; }
  .text-in:focus { outline: none; border-color: #4a5a7a; }
  .text-in.changed { border-color: #b85414; }
  .select { appearance: none; -webkit-appearance: none; padding-right: 28px; cursor: pointer; background: #fff url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' width='10' height='10' viewBox='0 0 10 10'%3E%3Cpath d='M2.5 4l2.5 2.5L7.5 4' fill='none' stroke='%235f6670' stroke-width='1.4' stroke-linecap='round' stroke-linejoin='round'/%3E%3C/svg%3E") no-repeat right 10px center; }
  .custom-in { margin-top: 6px; }
  .hint { margin-top: 4px; font-size: 11px; line-height: 1.5; color: #5f6670; }
  .changed-text { color: #b85414; }
  .link-btn { padding: 0; background: none; border: none; color: #4a5a7a; cursor: pointer; font: inherit; font-size: 12px; }
  .hint .link-btn { font-size: 11px; }
  .link-btn:hover { text-decoration: underline; }
  .seg { display: inline-flex; width: 240px; padding: 3px; gap: 2px; box-sizing: border-box; background: #ecebe5; border-radius: 7px; }
  .seg button { flex: 1; height: 26px; padding: 0; background: none; border: none; border-radius: 5px; color: #5f6670; cursor: pointer; font-size: 12px; }
  .seg button.on { background: #fff; color: #1f2328; font-weight: 600; box-shadow: 0 1px 2px rgba(0,0,0,.12); }
  .model-warn { margin: 6px 0 8px; padding: 9px 12px; border-radius: 7px; background: #fbefe6; color: #8a3f0e; font-size: 12px; line-height: 1.55; }
  .model-note { margin: 4px 0 8px; }
  .ver-row { display: grid; grid-template-columns: 110px 90px 1fr; gap: 12px; align-items: center; min-height: 30px; }
  .file-row { display: grid; grid-template-columns: 110px minmax(0, 1fr) auto; gap: 12px; align-items: center; min-height: 30px; }
  .ver-row > span:first-child, .file-row > span:first-child { color: #2b3240; }
  .ver-row .mono { font-size: 12px; }
  .file-row .mono { font-size: 11px; color: #5f6670; }
  .ok-text { color: #1f7a4d; font-size: 12px; }
  .bad-text { color: #b3362b; font-size: 12px; }
  .dialog-foot { display: flex; justify-content: flex-end; gap: 8px; padding: 14px 20px 16px; border-top: 1px solid rgba(0,0,0,.08); }
  .fresh { flex: 1; min-height: 0; overflow-y: auto; padding: 22px; }
  .fresh-inner { max-width: 860px; margin: 0 auto; display: flex; flex-direction: column; gap: 14px; }
  .fresh-title { margin: 4px 0 0; font-size: 22px; font-weight: 600; }
  .fresh .link-btn.small-text { font-size: 11px; }
  .prd-card { padding: 16px 18px; background: #fff; border: 1px solid rgba(0,0,0,.08); border-radius: 10px; }
  .prd-card p { margin: 0; }
  .sections { margin: 0; padding: 0; list-style: none; display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); column-gap: 24px; row-gap: 6px; font-size: 13px; color: #2b3240; }
  .key-row { display: flex; align-items: center; flex-wrap: wrap; gap: 8px; margin-top: 10px; padding: 9px 12px; border-radius: 7px; background: #f4f3ef; font-size: 13px; }
  .key-row.bad { background: none; border: 1px dashed #e3b5b0; }
  .key-mark { flex: none; }
  .key-bad { color: #b3362b; font-weight: 600; }
  .key-hint { font-size: 12px; }
  .key-copy { margin-left: auto; }
  .env-line { display: flex; align-items: center; gap: 8px; font-size: 13px; color: #5f6670; }
  .env-recheck { margin-left: auto; }
  .fresh-foot { display: flex; align-items: center; flex-wrap: wrap; gap: 12px; padding-top: 14px; border-top: 1px solid rgba(0,0,0,.08); }
  .flow { font-size: 13px; color: #5f6670; }
  .flow .faint { color: #8a8f96; }
  .fresh-go { margin-left: auto; display: inline-flex; align-items: center; gap: 10px; }
</style>
