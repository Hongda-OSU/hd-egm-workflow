<script lang="ts">
  import { onMount, tick } from "svelte";
  import {
    api,
    onProgress,
    resolveRef,
    type Advice,
    type AppStatus,
    type Issue,
    type PrdDetail,
    type PrdItem,
    type PrdList,
    type Role,
    type RuleItem,
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
  let message = $state<{ text: string; error: boolean } | null>(null);

  // Preflight: what a round needs. Nothing is shown while everything passes.
  type Problem = { text: string; cmd: string | null };
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
  const shown = $derived(
    triage ? triage.issues.filter((i) => src === "all" || i.source === src || i.also_from.includes(src)) : [],
  );
  const cur = $derived(triage?.issues.find((i) => i.id === issueId) ?? shown[0] ?? null);
  const undecided = $derived(
    triage ? triage.issues.filter((i) => !i.rejected_as && decisionOf(i.id) === null) : [],
  );
  // Once the latest round is handed off, starting the next one becomes the
  // page's main action; before that, deciding issues is.
  const readyForNext = $derived(!!detail?.rounds.at(-1)?.has_handoff);
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
        await api.decide(detail.page_id, round.n, cur.id, d, note.trim() || null);
        await loadDetail(true);
      } catch (e) {
        flash(String(e), true);
      }
    });
    return saving;
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
    const loaded = await api.getPrd(id);
    // Clicking through PRDs quickly can resolve loads out of order; a reply
    // for a PRD that is no longer selected must not replace the current one.
    if (selectedId !== id) return;
    detail = loaded;
    const last = Math.max(0, (detail?.rounds.length ?? 1) - 1);
    if (!keepPosition || roundIdx > last) roundIdx = last;
  }

  async function select(id: string) {
    selectedId = id;
    issueId = null;
    src = "all";
    try {
      await loadDetail(false);
    } catch (e) {
      flash(String(e), true);
    }
  }

  async function run(kind: "start" | "resume") {
    if (!selectedId || running) return;
    if (!(await checkStatus())) return;
    running = selectedId;
    progressLines = [];
    try {
      await (kind === "start" ? api.startRound(selectedId) : api.resumeRound(selectedId));
      flash("这一轮跑完了");
    } catch (e) {
      flash(String(e), true);
    } finally {
      running = null;
      issueId = null;
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
      await api.decide(detail.page_id, round.n, id, d, note.trim() || null);
      const at = shown.findIndex((i) => i.id === id);
      await loadDetail(true);
      issueId = id;
      if (advance) {
        const next = shown.slice(at + 1).find((i) => !i.rejected_as && decisionOf(i.id) === null);
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
      await api.makeHandoff(detail.page_id, round.n);
      flash("交接文档已生成并复制到剪贴板，贴给大象即可");
      await loadDetail(true);
    } catch (e) {
      flash(String(e), true);
    }
  }

  async function copyHandoff() {
    if (!detail || !round) return;
    try {
      await api.copyHandoff(detail.page_id, round.n);
      flash("交接文档已复制到剪贴板");
    } catch (e) {
      flash(String(e), true);
    }
  }

  onMount(() => {
    const unlisten = onProgress((p) => {
      if (p.page_id === running) progressLines = [...progressLines, p.line];
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

{#snippet dots()}
  <span class="dots" aria-hidden="true"><span></span><span></span><span></span></span>
{/snippet}

<div class="app">
  <header class="top">
    <div class="brand">
      <span class="name">hd-egm-workflow</span>
      <span class="muted">Claude 写 PRD，GPT 零上下文审稿</span>
    </div>
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
                      <span class="pill review">第 {p.rounds} 轮</span>
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
          <h1>{selectedItem?.title}</h1>
          <p class="muted">4 条金鱼并行审稿，整理员随后汇总，通常 2～3 分钟。</p>
          <ol class="log">
            {#each progressLines as line}<li>{line}</li>{/each}
          </ol>
        </section>
      {:else if !detail || detail.rounds.length === 0}
        <section class="empty">
          <div class="muted">还没审过</div>
          <h1>{selectedItem?.title ?? "选一份 PRD"}</h1>
          {#if selectedItem}
            <ol class="first-steps">
              <li>从 Notion 拉取最新版本，存成快照</li>
              <li>4 条金鱼各开一个零记忆会话审稿</li>
              <li>整理员把结果归类，交给你拍板</li>
            </ol>
            <button type="button" class="secondary big-btn" disabled={!!running || blocked} onclick={() => run("start")}>
              开始第 1 轮
              <svg width="12" height="12" viewBox="0 0 12 12" aria-hidden="true"><path d="M2 6h8M7 3l3 3-3 3" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" /></svg>
            </button>
            {#if blocked}<span class="blocked-hint">先解决上面的问题</span>{:else}<span class="muted small-text">通常 2～3 分钟，用的是订阅额度</span>{/if}
          {/if}
        </section>
      {:else}
        <div class="center-head">
          <div>
            <div class="muted small-text">审稿中的 PRD</div>
            <h1>{detail.title}</h1>
          </div>
          {#if !readyForNext}
            <div class="start">
              <button type="button" class="secondary" title="先从 Notion 拉取最新版本，再开始审稿"
                disabled={!!running || blocked} onclick={() => run("start")}>
                <span aria-hidden="true">＋</span>开始第 {nextRound} 轮
              </button>
              {#if blocked}<span class="blocked-hint">先解决上面的问题</span>{/if}
            </div>
          {/if}
        </div>

        <div class="tabs" role="tablist" aria-label="轮次">
          {#each detail.rounds as r, i (r.n)}
            <button type="button" role="tab" aria-selected={i === roundIdx} class="tab" class:on={i === roundIdx}
              onclick={() => { roundIdx = i; issueId = null; src = "all"; }}>
              <span class="tab-top">第 {r.n} 轮
                {#if r.verdicts}<span class="verdict" style={verdictStyle(r.verdicts.prd)}>{r.verdicts.prd}</span>{/if}
              </span>
              <span class="muted small-text">{roundSummary(r)}</span>
            </button>
          {/each}
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
          <div class="verdict-card">
            <span class="verdict-big" class:go={prd === "GO"}>{prd ?? "—"}</span>
            {#if verdictNote}<span class="muted verdict-note">{verdictNote}</span>{/if}
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
  .first-steps { margin: 0; padding: 14px 18px; list-style: none; counter-reset: step; display: flex; flex-direction: column; gap: 8px; text-align: left; background: #fff; border: 1px solid rgba(0,0,0,.08); border-radius: 10px; color: #2b3240; }
  .first-steps li { counter-increment: step; display: flex; align-items: baseline; gap: 10px; }
  .first-steps li::before { content: counter(step); flex: none; width: 18px; height: 18px; display: inline-grid; place-items: center; border-radius: 50%; background: #ecebe5; color: #5f6670; font-size: 11px; font-weight: 600; }
  .big-btn { min-height: 40px; padding: 0 18px; gap: 8px; border-color: rgba(0,0,0,.22); border-radius: 9px; font-weight: 600; box-shadow: 0 1px 2px rgba(0,0,0,.06); }
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
  .resume { display: flex; align-items: center; gap: 12px; margin-top: 14px; }
  .log { width: min(560px, 100%); max-height: 50vh; overflow-y: auto; text-align: left; margin: 0; padding: 12px 12px 12px 32px; background: #fff; border: 1px solid rgba(0,0,0,.08); border-radius: 8px; font-family: ui-monospace, "SF Mono", Menlo, monospace; font-size: 12px; line-height: 1.8; }

  .tabs { flex: none; display: flex; padding: 0 22px 14px; overflow-x: auto; }
  .tab { flex: 1 1 0; min-width: 150px; min-height: 58px; display: flex; flex-direction: column; justify-content: center; gap: 4px; padding: 8px 14px; text-align: left; background: #f9f8f4; border: 1px solid rgba(0,0,0,.1); margin-right: -1px; color: #1f2328; cursor: pointer; }
  .tab.on { background: #fff; border-color: #1f2328; position: relative; z-index: 1; }
  .tab-top { display: flex; justify-content: space-between; width: 100%; font-weight: 600; }
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
  .verdict-card { display: flex; align-items: center; gap: 10px; padding: 12px 14px; background: #fff; border: 1px solid rgba(0,0,0,.08); border-radius: 10px; }
  .verdict-big { flex: none; font-size: 15px; font-weight: 700; color: #b3362b; }
  .verdict-big.go { color: #1f7a4d; }
  .verdict-note { font-size: 12px; line-height: 1.5; }
  .line { padding: 8px 0; border-bottom: 1px solid rgba(0,0,0,.06); }
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
</style>
