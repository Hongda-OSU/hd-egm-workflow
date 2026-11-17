<script lang="ts">
  import { onMount } from "svelte";
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
  const FISH_NAMES: Record<string, string> = { retell: "复述", critic: "找茬", ready: "开工", spec: "规范" };
  const STEP_LABEL: Record<string, string> = { queued: "排队", running: "运行中", done: "完成", failed: "失败" };

  let status = $state<AppStatus | null>(null);
  let prds = $state<PrdList | null>(null);
  let rules = $state<RuleItem[]>([]);
  let selectedId = $state<string | null>(null);
  let detail = $state<PrdDetail | null>(null);
  let roundIdx = $state(0);
  let src = $state<"all" | Role>("all");
  let issueId = $state<string | null>(null);
  let note = $state("");
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
  });

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

  function verdictStyle(v: Verdict | undefined) {
    return v === "GO" ? "color:#1f7a4d;background:#e6f4ec" : "color:#b3362b;background:#fbeae8";
  }

  function roundSummary(r: PrdDetail["rounds"][number]) {
    if (!r.triage) return r.stage === "failed" ? "没跑完" : "只拉了快照";
    const t = r.triage;
    return `${t.issues.length} 个问题 · 命中 ${t.key_decision_hits}/${t.key_decision_total}`;
  }

  async function loadList() {
    try {
      [prds, rules] = await Promise.all([api.listPrds(), api.rulePages()]);
      if (!selectedId && prds.items.length) await select(prds.items[0].page_id);
    } catch (e) {
      flash(String(e), true);
    }
  }

  async function loadDetail(keepPosition: boolean) {
    if (!selectedId) return;
    detail = await api.getPrd(selectedId);
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
    if (!detail || !round || !cur) return;
    try {
      await api.decide(detail.page_id, round.n, cur.id, d, note.trim() || null);
      const at = shown.findIndex((i) => i.id === cur!.id);
      await loadDetail(true);
      const next = shown.slice(at + 1).find((i) => !i.rejected_as && decisionOf(i.id) === null);
      if (next) issueId = next.id;
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

  onMount(() => {
    const unlisten = onProgress((p) => {
      if (p.page_id === running) progressLines = [...progressLines, p.line];
    });
    checkStatus();
    loadList();
    // Coming back from a terminal login should clear the banner by itself.
    const onFocus = () => checkStatus();
    window.addEventListener("focus", onFocus);
    return () => {
      unlisten.then((f) => f());
      window.removeEventListener("focus", onFocus);
    };
  });
</script>

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
        <button type="button" class="icon-btn" onclick={loadList} title="刷新列表" aria-label="刷新列表">↻</button>
      </div>
      <div class="search">
        <input type="search" placeholder="搜索 PRD" aria-label="搜索 PRD" bind:value={query} />
      </div>
      {#if prds?.offline}
        <p class="offline">Notion 连不上，只显示本地已有的 PRD。</p>
      {/if}
      <div class="prd-list">
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
      {#if running === selectedId}
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
            <p class="muted">拉取 Notion 里的最新版本存成快照，4 条金鱼各开一个零记忆会话审稿，结果交给整理员归类。</p>
            <button type="button" class="primary big" disabled={!!running || blocked} onclick={() => run("start")}>开始第 1 轮</button>
            {#if blocked}<span class="blocked-hint">先解决上面的问题</span>{/if}
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
              <p>这一轮没跑完。</p>
              <ul>
                {#each Object.entries(round.fish_steps) as [name, s]}
                  <li>{FISH_NAMES[name]}金鱼：{STEP_LABEL[s.status]}{s.error ? ` — ${s.error}` : ""}</li>
                {/each}
                <li>整理员：{STEP_LABEL[round.triage_step.status]}{round.triage_step.error ? ` — ${round.triage_step.error}` : ""}</li>
              </ul>
              <button type="button" class="primary" disabled={!!running || blocked} onclick={() => run("resume")}>继续跑没完成的部分</button>
              {#if blocked}<span class="blocked-hint">先解决上面的问题</span>{/if}
            {:else}
              <p class="muted">这一轮只拉了快照，没有审稿结果。</p>
            {/if}
          </section>
        {:else if round && triage}
          <div class="panes">
            <section class="card list">
              <div class="chips">
                {#each ["all", "retell", "critic", "ready"] as k}
                  {@const n = k === "all" ? triage.issues.length : triage.issues.filter((i) => i.source === k || i.also_from.includes(k as Role)).length}
                  <button type="button" class="chip" class:on={src === k} onclick={() => { src = k as typeof src; issueId = null; }}>
                    {k === "all" ? "全部" : ROLE[k as Role].label} {n}
                  </button>
                {/each}
              </div>
              <div class="issues">
                {#each shown as it (it.id)}
                  {@const st = stateOf(it)}
                  <button type="button" class="issue" class:on={cur?.id === it.id} onclick={() => (issueId = it.id)}>
                    <span class="issue-meta">
                      <span class="role" style="color:{ROLE[it.source].color};background:{ROLE[it.source].bg}">{ROLE[it.source].label}{it.also_from.length ? ` +${it.also_from.length}` : ""}</span>
                      <span class="sev" style="color:{SEV[it.severity].color}">{SEV[it.severity].label}</span>
                      <span class="muted ellipsis">{it.section}</span>
                      <span class="state" style="color:{st.color}">{st.text}</span>
                    </span>
                    <span class="issue-title">{it.title}</span>
                  </button>
                {:else}
                  <p class="muted center-text">这一类没有问题。</p>
                {/each}
              </div>
              {#if round.latest}
                <div class="list-foot">
                  <span class="muted small-text">
                    {undecided.length ? `还有 ${undecided.length} 条待决定` : round.has_handoff ? "交接文档已生成" : "都拍完了"}
                  </span>
                  <button type="button" class={round.has_handoff ? "secondary" : "primary"} disabled={undecided.length > 0} onclick={handoff}>
                    {round.has_handoff ? "重新生成交接文档" : "生成交接文档"}
                  </button>
                </div>
              {/if}
            </section>

            <section class="card detail">
              {#if cur}
                {@const st = stateOf(cur)}
                <div class="issue-meta">
                  {#each [cur.source, ...cur.also_from] as r}
                    <span class="role" style="color:{ROLE[r].color};background:{ROLE[r].bg}">{ROLE[r].label}金鱼</span>
                  {/each}
                  <span class="sev" style="color:{SEV[cur.severity].color}">严重度 {SEV[cur.severity].label}</span>
                  <span class="muted">{cur.section}</span>
                </div>
                <h2>{cur.title}</h2>
                <div class="grid">
                  <span class="muted">问题</span><span>{cur.problem}</span>
                  <span class="muted">影响</span><span>{cur.impact}</span>
                </div>
                <div class="advice">
                  <div class="advice-head">整理员建议：{cur.advice === "accept" ? "接受" : "驳回"}</div>
                  <p>{cur.reason}</p>
                  {#if cur.suggested_change}<p><span class="muted">建议改法：</span>{cur.suggested_change}</p>{/if}
                </div>
                {#if cur.rejected_as}
                  <p class="auto">这条和历史驳回的 {cur.rejected_as} 是同一个问题，已自动驳回，不用再拍板；判断错了可以改成接受。</p>
                {/if}
                <label class="note-label" for="note">备注（会写进交接文档）</label>
                <textarea id="note" rows="2" bind:value={note} disabled={!round.latest} placeholder="可选"></textarea>
                <div class="decide">
                  <span class="muted">我的决定</span>
                  <button type="button" class="choice accept" class:on={decisionOf(cur.id) === "accept"} disabled={!round.latest} onclick={() => decide("accept")}>接受</button>
                  <button type="button" class="choice reject" class:on={decisionOf(cur.id) === "reject"} disabled={!round.latest} onclick={() => decide("reject")}>驳回</button>
                  <span class="muted small-text">{round.latest ? `当前：${st.text}` : "历史轮次只读"}</span>
                </div>
                {#if cur.refs.length}
                  <details class="refs">
                    <summary>金鱼原文（{cur.refs.length} 条）</summary>
                    {#each cur.refs as ref}
                      <div class="ref"><span class="mono">{ref}</span><p>{resolveRef(round.fish, ref) ?? "（找不到原文）"}</p></div>
                    {/each}
                  </details>
                {/if}
              {:else}
                <p class="muted center-text">选一条问题。</p>
              {/if}
            </section>
          </div>
        {/if}
      {/if}
    </main>

    <aside class="right">
      {#if round?.triage}
        {@const t = round.triage}
        <section>
          <h2 class="label">第 {round.n} 轮 · 三条金鱼</h2>
          {#each [["复述金鱼", round.verdicts?.retell], ["找茬金鱼", round.verdicts?.critic], ["开工金鱼", round.verdicts?.ready]] as [name, v]}
            <div class="row"><span>{name}</span><span class="verdict" style={verdictStyle(v as Verdict)}>{v ?? "—"}</span></div>
          {/each}
          <div class="row strong"><span>整份 PRD</span><span class="verdict" style={verdictStyle(round.verdicts?.prd)}>{round.verdicts?.prd ?? "—"}</span></div>
          {#each round.warnings as w}<p class="warn">{w}</p>{/each}
        </section>
        <section>
          <h2 class="label">关键决策命中率</h2>
          <div class="big-num">{t.key_decision_hits}<span class="muted"> / {t.key_decision_total} 条</span></div>
          <p class="muted small-text">复述金鱼说对了关键决策清单里的几条，部分说对记 0.5；≥ 90% 才达标。</p>
          {#each t.key_decisions.filter((k) => k.result !== "hit") as k}
            <p class="kd"><span style="color:{k.result === 'miss' ? '#b3362b' : '#a86b00'}">{k.result === "miss" ? "漏" : "半"}</span>{k.item}</p>
          {/each}
        </section>
        <section>
          <h2 class="label">每轮问题数</h2>
          <div class="bars">
            {#each detail?.rounds.filter((r) => r.triage) ?? [] as r}
              {@const c = r.triage!.issues.length}
              <div class="bar">
                <span class="strong">{c}</span>
                <span class="fill" style="height:{Math.max(6, Math.round((c / maxIssues) * 64))}px;background:{r.n === round.n ? '#b85414' : '#d9d6cd'}" aria-hidden="true"></span>
                <span class="muted small-text">R{r.n}</span>
              </div>
            {/each}
          </div>
        </section>
        <section>
          <h2 class="label">规范达标 · {t.spec_checks.filter((c) => c.pass).length} / {t.spec_checks.length}</h2>
          {#each t.spec_checks as c}
            <div class="spec"><span style="color:{c.pass ? '#1f7a4d' : '#b3362b'}" aria-hidden="true">{c.pass ? "✓" : "✕"}</span><span>{c.item}</span></div>
          {/each}
        </section>
      {:else}
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
  .rules { flex: none; padding: 12px 16px 16px; border-top: 1px solid rgba(0,0,0,.08); }
  .rule { width: 100%; display: flex; justify-content: space-between; min-height: 26px; align-items: center; padding: 0; background: none; border: none; color: #4a5a7a; cursor: pointer; font-family: ui-monospace, "SF Mono", Menlo, monospace; font-size: 11px; }
  .rule:disabled { cursor: default; color: #8a8f96; }

  .center { flex: 1; min-width: 0; min-height: 0; display: flex; flex-direction: column; }
  .center-head { flex: none; display: flex; justify-content: space-between; align-items: center; gap: 12px; padding: 16px 22px 12px; }
  h1 { margin: 4px 0 0; font-size: 22px; font-weight: 600; }
  h2 { font-size: 16px; font-weight: 600; line-height: 1.5; margin: 10px 0 0; }
  .primary { min-height: 38px; padding: 0 16px; background: #1f2328; color: #fff; border: none; border-radius: 8px; cursor: pointer; font-weight: 500; }
  .primary:disabled { background: #b9b6ae; cursor: default; }
  .primary.big { min-height: 44px; padding: 0 20px; }
  .empty, .running { flex: 1; display: flex; flex-direction: column; align-items: center; justify-content: center; gap: 14px; padding: 40px; text-align: center; }
  .empty p { max-width: 420px; line-height: 1.6; margin: 0; }
  .log { width: min(560px, 100%); max-height: 50vh; overflow-y: auto; text-align: left; margin: 0; padding: 12px 12px 12px 32px; background: #fff; border: 1px solid rgba(0,0,0,.08); border-radius: 8px; font-family: ui-monospace, "SF Mono", Menlo, monospace; font-size: 12px; line-height: 1.8; }

  .tabs { flex: none; display: flex; padding: 0 22px 14px; overflow-x: auto; }
  .tab { flex: 1 1 0; min-width: 150px; min-height: 58px; display: flex; flex-direction: column; justify-content: center; gap: 4px; padding: 8px 14px; text-align: left; background: #f9f8f4; border: 1px solid rgba(0,0,0,.1); margin-right: -1px; color: #1f2328; cursor: pointer; }
  .tab.on { background: #fff; border-color: #1f2328; position: relative; z-index: 1; }
  .tab-top { display: flex; justify-content: space-between; width: 100%; font-weight: 600; }
  .verdict { font-size: 11px; font-weight: 600; border-radius: 4px; padding: 2px 8px; }
  .notice { margin: 0 22px; padding: 16px 20px; background: #fff; border: 1px solid rgba(0,0,0,.08); border-radius: 10px; line-height: 1.7; }

  .panes { flex: 1; min-height: 0; display: flex; gap: 14px; padding: 0 22px 18px; }
  .card { min-width: 0; display: flex; flex-direction: column; background: #fff; border: 1px solid rgba(0,0,0,.08); border-radius: 10px; min-height: 0; }
  .list { flex: 1 1 0; }
  .detail { flex: 1.15 1 0; overflow-y: auto; padding: 18px 20px; }
  .chips { flex: none; display: flex; flex-wrap: wrap; gap: 6px; padding: 12px 14px; border-bottom: 1px solid rgba(0,0,0,.06); }
  .chip { min-height: 30px; padding: 0 10px; border-radius: 999px; border: 1px solid rgba(0,0,0,.12); background: #fff; color: #2b3240; cursor: pointer; font-size: 12px; }
  .chip.on { background: #1f2328; color: #fff; border-color: #1f2328; }
  .issues { flex: 1; min-height: 0; overflow-y: auto; padding: 8px; }
  .issue { width: 100%; display: flex; flex-direction: column; gap: 6px; padding: 12px; margin-bottom: 6px; text-align: left; background: #fff; border: 1px solid rgba(0,0,0,.08); border-radius: 8px; color: #1f2328; cursor: pointer; }
  .issue.on { background: #f4f5f8; border-color: #4a5a7a; }
  .issue-meta { display: flex; flex-wrap: wrap; align-items: center; gap: 8px; width: 100%; font-size: 11px; }
  .role { font-weight: 600; border-radius: 4px; padding: 2px 7px; }
  .sev { font-weight: 600; }
  .state { margin-left: auto; }
  .issue-title { font-size: 13px; line-height: 1.5; }
  .list-foot { flex: none; display: flex; justify-content: space-between; align-items: center; gap: 8px; padding: 10px 14px; border-top: 1px solid rgba(0,0,0,.06); }

  .grid { margin-top: 12px; display: grid; grid-template-columns: 56px minmax(0, 1fr); gap: 8px 12px; line-height: 1.6; }
  .advice { margin-top: 16px; padding: 12px 14px; background: #f4f5f8; border-radius: 8px; }
  .advice-head { font-size: 11px; font-weight: 600; color: #4a5a7a; }
  .advice p { margin: 6px 0 0; line-height: 1.6; color: #2b3240; }
  .auto { margin: 12px 0 0; padding: 10px 12px; background: #fbfaf7; border-radius: 8px; color: #5f6670; line-height: 1.6; }
  .note-label { display: block; margin-top: 14px; font-size: 11px; color: #5f6670; }
  textarea { width: 100%; box-sizing: border-box; margin-top: 4px; padding: 8px 10px; border: 1px solid rgba(0,0,0,.14); border-radius: 8px; font: inherit; resize: vertical; }
  .decide { display: flex; flex-wrap: wrap; align-items: center; gap: 8px; margin-top: 10px; }
  .choice { min-height: 36px; padding: 0 14px; border-radius: 8px; background: #fff; cursor: pointer; font-weight: 500; }
  .choice.accept { border: 1px solid #1f7a4d; color: #1f7a4d; }
  .choice.accept.on { background: #1f7a4d; color: #fff; }
  .choice.reject { border: 1px solid #5f6670; color: #5f6670; }
  .choice.reject.on { background: #5f6670; color: #fff; }
  .choice:disabled { opacity: .5; cursor: default; }
  .refs { margin-top: 18px; }
  .refs summary { cursor: pointer; font-size: 12px; font-weight: 600; }
  .ref { margin-top: 8px; padding: 8px 10px; background: #fbfaf7; border-radius: 6px; }
  .ref p { margin: 4px 0 0; line-height: 1.6; }

  .right { flex: 0 0 272px; min-height: 0; overflow-y: auto; background: #fbfaf7; border-left: 1px solid rgba(0,0,0,.08); padding: 16px; display: flex; flex-direction: column; gap: 18px; }
  .right section { display: flex; flex-direction: column; }
  .row { display: flex; justify-content: space-between; align-items: center; min-height: 32px; border-bottom: 1px solid rgba(0,0,0,.06); }
  .warn { margin: 6px 0 0; font-size: 11px; color: #a86b00; }
  .big-num { font-size: 32px; font-weight: 600; }
  .big-num .muted { font-size: 13px; font-weight: 400; }
  .kd { display: flex; gap: 6px; margin: 4px 0 0; font-size: 11px; line-height: 1.5; color: #2b3240; }
  .kd span { flex: none; font-weight: 700; }
  .bars { display: flex; align-items: flex-end; gap: 14px; height: 92px; }
  .bar { flex: 1; display: flex; flex-direction: column; align-items: center; justify-content: flex-end; gap: 4px; height: 100%; }
  .fill { width: 100%; border-radius: 4px 4px 0 0; }
  .spec { display: flex; gap: 8px; align-items: flex-start; min-height: 24px; font-size: 12px; line-height: 1.5; }
  .spec span:first-child { flex: none; width: 14px; font-weight: 700; }
</style>
