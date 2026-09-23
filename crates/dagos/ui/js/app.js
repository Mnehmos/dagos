// Browser glue: holds UI state (what is selected), loads recorded state from the API, follows the
// live event stream, and wires input. All DAGOS semantics stay in the runtime; rendering lives in
// view.js and derivations in model.js.

import { api, openStream } from "./api.js";
import * as model from "./model.js";
import * as view from "./view.js";

const $ = (id) => document.getElementById(id);
const TABS = view.TABS.map((tab) => tab.key);
const THEMES = ["system", "light", "dark"];

const state = {
  overview: null,
  detail: null,
  selectedRunId: null,
  /** Follow new runs as they start, unless the user is looking at an older one. */
  follow: true,
  tab: "conversation",
  selectedNodeId: null,
  /** Events of the run streaming now, as they arrive: `{ runId, events, streamed }`. */
  live: null,
  connection: "connecting",
  configOpen: false,
  bannerDismissed: null,
};

// ---------------------------------------------------------------------------------------------
// Loading

async function loadOverview() {
  state.overview = await api.overview();
  renderChrome();
  renderRuns();
  renderDag();
  renderStatus();
}

async function loadDetail(runId, { keepScroll = false } = {}) {
  if (!runId) {
    state.detail = null;
    renderInspector();
    renderDag();
    return;
  }
  const detail = await api.run(runId);
  if (runId !== state.selectedRunId) return;
  state.detail = detail;
  renderInspector({ keepScroll });
  renderDag();
}

function newestRunId() {
  return state.overview?.runs.at(-1)?.run.id ?? null;
}

function selectRun(runId) {
  if (runId !== state.selectedRunId) {
    state.selectedRunId = runId;
    state.detail = null;
    state.selectedNodeId = null;
  }
  state.follow = runId === newestRunId();
  history.replaceState(null, "", runId ? `#run=${runId}` : location.pathname);
  renderRuns();
  renderInspector();
  loadDetail(runId).catch(showError);
}

let refreshTimer = null;
function scheduleRefresh() {
  clearTimeout(refreshTimer);
  refreshTimer = setTimeout(async () => {
    try {
      await loadOverview();
      if (state.selectedRunId) await loadDetail(state.selectedRunId, { keepScroll: true });
    } catch (error) {
      showError(error);
    }
  }, 120);
}

// ---------------------------------------------------------------------------------------------
// Live events

function onEvent(event) {
  state.live = model.applyLiveEvent(state.live?.runId === event.run_id ? state.live : null, event);
  if (event.type === "run.started" && (state.follow || !state.selectedRunId)) {
    state.selectedRunId = event.run_id;
    state.detail = null;
    state.tab = "conversation";
    history.replaceState(null, "", `#run=${event.run_id}`);
  }
  if (event.type === "inference.delta") {
    if (event.run_id === state.selectedRunId) showStreamedProse();
    return;
  }
  scheduleRefresh();
}

function showStreamedProse() {
  const prose = $("live-prose");
  if (prose && state.live) {
    prose.textContent = state.live.streamed;
  } else {
    scheduleRefresh();
  }
}

function resync() {
  loadOverview()
    .then(() => {
      if (!state.selectedRunId || state.follow) {
        const newest = newestRunId();
        if (newest !== state.selectedRunId) return selectRun(newest);
      }
      return state.selectedRunId && loadDetail(state.selectedRunId, { keepScroll: true });
    })
    .catch(showError);
}

// ---------------------------------------------------------------------------------------------
// Rendering

function renderChrome() {
  const overview = state.overview;
  if (!overview) return;
  $("project-name").textContent = overview.project.name;
  document.title = `${overview.project.name} · DAGOS`;
  const defaults = overview.run_defaults;
  $("config-provider-chip").textContent = defaults.provider_id;
  $("config-model-chip").textContent = defaults.model_id;
  if (!state.configOpen) fillConfigForm();

  const stale = model.runList(overview).filter((run) => run.stale);
  if (stale.length && state.bannerDismissed !== "stale") {
    showBanner(
      "warn",
      `${stale.length} run(s) are marked running, but no DAGOS process is executing them (the server probably restarted).`,
      { label: "Mark as interrupted", action: "recover" },
    );
  } else if ($("banner").dataset.kind === "warn") {
    hideBanner();
  }
}

function renderRuns() {
  if (!state.overview) return;
  $("run-list").innerHTML = view.runListHtml(model.runList(state.overview), state.selectedRunId);
}

function renderInspector({ keepScroll = false } = {}) {
  const root = $("inspector");
  const panel = root.querySelector("#panel");
  const scrollTop = keepScroll && panel ? panel.scrollTop : 0;
  const openDetails = keepScroll
    ? [...root.querySelectorAll("details[open] > summary")].map((summary) => summary.textContent)
    : [];
  const focusedTab = document.activeElement?.closest?.("[role=tab]")?.dataset.tab;

  if (!state.overview) {
    root.innerHTML = `<p class="loading">Loading…</p>`;
    return;
  }
  if (!state.selectedRunId) {
    root.innerHTML = view.emptyInspectorHtml(state.overview);
    return;
  }
  if (!state.detail) {
    root.innerHTML = `<p class="loading">Loading run…</p>`;
    return;
  }
  const live = state.live?.runId === state.detail.run.id ? state.live : null;
  const recorded = state.detail.streamed ?? "";
  const streamed = live && live.streamed.length > recorded.length ? live.streamed : recorded;
  root.innerHTML = view.inspectorHtml({
    detail: state.detail,
    dag: state.overview.dag,
    tab: state.tab,
    streamed,
    streaming: state.detail.run.status === "running",
  });
  const newPanel = root.querySelector("#panel");
  if (newPanel) newPanel.scrollTop = scrollTop;
  for (const summary of root.querySelectorAll("details > summary")) {
    if (openDetails.includes(summary.textContent)) summary.parentElement.open = true;
  }
  if (focusedTab) root.querySelector(`[role=tab][data-tab="${focusedTab}"]`)?.focus();
}

function renderDag() {
  if (!state.overview) return;
  const members = state.detail?.context ?? [];
  $("dag").innerHTML = view.dagHtml({
    dag: state.overview.dag,
    active: new Set(members.map((member) => member.node_id)),
    emitted: new Set(state.detail?.emitted_nodes ?? []),
    selectedNodeId: state.selectedNodeId,
    runId: state.selectedRunId,
    memberSources: new Map(members.map((member) => [member.node_id, member.source])),
  });
}

function renderStatus() {
  $("status").innerHTML = view.statusHtml(state.overview, state.connection);
  const indicator = $("connection");
  indicator.dataset.state = state.connection;
  indicator.textContent = { live: "Live", connecting: "Connecting…", offline: "Reconnecting…" }[state.connection];
}

// ---------------------------------------------------------------------------------------------
// Banner and toast

function showBanner(kind, message, action) {
  const banner = $("banner");
  banner.dataset.kind = kind;
  banner.innerHTML = `<span>${view.esc(message)}</span>
    ${action ? `<button type="button" data-action="${view.esc(action.action)}">${view.esc(action.label)}</button>` : ""}
    <button type="button" class="icon-button" data-action="dismiss" aria-label="Dismiss">×</button>`;
  banner.hidden = false;
}

function hideBanner() {
  const banner = $("banner");
  banner.hidden = true;
  delete banner.dataset.kind;
}

function showError(error) {
  showBanner("error", error?.message ?? String(error));
}

let toastTimer = null;
function toast(message) {
  const element = $("toast");
  element.textContent = message;
  element.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => (element.hidden = true), 2600);
}

// ---------------------------------------------------------------------------------------------
// Actions

function setTab(tab, { focus = false } = {}) {
  if (!TABS.includes(tab)) return;
  state.tab = tab;
  renderInspector();
  if (focus) $("inspector").querySelector(`[role=tab][data-tab="${tab}"]`)?.focus();
}

function selectNode(nodeId) {
  state.selectedNodeId = nodeId;
  renderDag();
  if (nodeId) $("dag").querySelector(".node-detail")?.scrollIntoView({ block: "nearest" });
}

function moveRun(step) {
  const runs = model.runList(state.overview ?? { runs: [] });
  if (!runs.length) return;
  const index = runs.findIndex((run) => run.id === state.selectedRunId);
  const next = runs[Math.min(runs.length - 1, Math.max(0, (index === -1 ? 0 : index) + step))];
  selectRun(next.id);
  $("run-list").querySelector(`[data-run="${next.id}"]`)?.focus();
}

async function recover() {
  try {
    const { recovered } = await api.recover();
    hideBanner();
    toast(`Marked ${recovered.length} run(s) as interrupted.`);
    await loadOverview();
    if (state.selectedRunId) await loadDetail(state.selectedRunId, { keepScroll: true });
  } catch (error) {
    showError(error);
  }
}

async function copyIr() {
  try {
    await navigator.clipboard.writeText(JSON.stringify(state.detail?.ir ?? null, null, 2));
    toast("IR JSON copied.");
  } catch {
    toast("Copy failed; select the JSON and copy it manually.");
  }
}

async function sendMessage(event) {
  event.preventDefault();
  const input = $("message");
  const message = input.value.trim();
  if (!message) return;
  const button = $("send");
  button.disabled = true;
  button.textContent = "Starting…";
  try {
    const { run } = await api.start({ message });
    input.value = "";
    state.tab = "conversation";
    state.follow = true;
    state.selectedRunId = run.id;
    state.detail = null;
    history.replaceState(null, "", `#run=${run.id}`);
    await loadOverview();
    await loadDetail(run.id);
  } catch (error) {
    showError(error);
  } finally {
    button.disabled = false;
    button.textContent = "Run";
  }
}

// ---------------------------------------------------------------------------------------------
// Configuration

function fillConfigForm() {
  const overview = state.overview;
  if (!overview) return;
  $("config-provider").innerHTML = overview.providers
    .map((provider) => `<option value="${view.esc(provider.id)}">${view.esc(provider.id)}</option>`)
    .join("");
  $("config-provider").value = overview.run_defaults.provider_id;
  fillModelSuggestions();
  $("config-model").value = overview.run_defaults.model_id;
  $("config-prompt").value = overview.run_defaults.system_prompt;
}

function fillModelSuggestions() {
  const provider = state.overview?.providers.find((candidate) => candidate.id === $("config-provider").value);
  $("config-models").innerHTML = (provider?.suggested_models ?? [])
    .map((modelId) => `<option value="${view.esc(modelId)}"></option>`)
    .join("");
}

function toggleConfig(open = !state.configOpen) {
  state.configOpen = open;
  $("config-panel").hidden = !open;
  $("config-toggle").setAttribute("aria-expanded", String(open));
  if (open) {
    fillConfigForm();
    $("config-provider").focus();
  }
}

async function saveConfig(event) {
  event.preventDefault();
  try {
    await api.saveConfig({
      provider_id: $("config-provider").value,
      model_id: $("config-model").value.trim(),
      system_prompt: $("config-prompt").value,
    });
    toggleConfig(false);
    await loadOverview();
    toast("Saved. Subsequent runs use this configuration.");
    $("config-toggle").focus();
  } catch (error) {
    showError(error);
  }
}

// ---------------------------------------------------------------------------------------------
// Theme

function savedTheme() {
  try {
    return localStorage.getItem("dagos-theme") ?? "system";
  } catch {
    return "system";
  }
}

function applyTheme(theme) {
  if (theme === "system") delete document.documentElement.dataset.theme;
  else document.documentElement.dataset.theme = theme;
  try {
    localStorage.setItem("dagos-theme", theme);
  } catch {
    // Theme preference is a convenience; losing it is harmless.
  }
  const toggle = $("theme-toggle");
  toggle.setAttribute("aria-label", `Color theme: ${theme}. Change theme`);
  toggle.title = `Theme: ${theme} (t)`;
}

function cycleTheme() {
  const current = savedTheme();
  applyTheme(THEMES[(THEMES.indexOf(current) + 1) % THEMES.length]);
}

// ---------------------------------------------------------------------------------------------
// Input wiring

function onClick(event) {
  const target = event.target.closest("[data-run], [data-tab], [data-node], [data-copy], [data-close-node], [data-action]");
  if (!target) return;
  const data = target.dataset;
  if (data.run) selectRun(data.run);
  else if (data.tab) setTab(data.tab);
  else if (data.node) selectNode(data.node === state.selectedNodeId ? null : data.node);
  else if (data.copy === "ir") copyIr();
  else if ("closeNode" in data) selectNode(null);
  else if (data.action === "recover") recover();
  else if (data.action === "dismiss") {
    state.bannerDismissed = $("banner").dataset.kind === "warn" ? "stale" : state.bannerDismissed;
    hideBanner();
  }
}

function onKeyDown(event) {
  if (event.key === "Escape") {
    if (state.configOpen) toggleConfig(false);
    else if (state.selectedNodeId) selectNode(null);
    return;
  }
  const tab = event.target.closest?.("[role=tab]");
  if (tab && (event.key === "ArrowRight" || event.key === "ArrowLeft")) {
    event.preventDefault();
    const index = TABS.indexOf(tab.dataset.tab) + (event.key === "ArrowRight" ? 1 : -1);
    setTab(TABS[(index + TABS.length) % TABS.length], { focus: true });
    return;
  }
  const typing = event.target.closest?.("input, textarea, select, [contenteditable]");
  if (typing || event.metaKey || event.ctrlKey || event.altKey) return;
  if (/^[1-6]$/.test(event.key)) {
    setTab(TABS[Number(event.key) - 1]);
    return;
  }
  const actions = {
    "/": () => $("message").focus(),
    n: () => $("message").focus(),
    j: () => moveRun(1),
    k: () => moveRun(-1),
    c: () => toggleConfig(),
    t: () => cycleTheme(),
    "?": () => $("help").showModal(),
  };
  const action = actions[event.key];
  if (action) {
    event.preventDefault();
    action();
  }
}

function bind() {
  document.addEventListener("click", onClick);
  document.addEventListener("keydown", onKeyDown);
  $("composer").addEventListener("submit", sendMessage);
  $("message").addEventListener("keydown", (event) => {
    if (event.key === "Enter" && !event.shiftKey && !event.isComposing) {
      event.preventDefault();
      $("composer").requestSubmit();
    }
  });
  $("config-toggle").addEventListener("click", () => toggleConfig());
  $("config-cancel").addEventListener("click", () => toggleConfig(false));
  $("config-form").addEventListener("submit", saveConfig);
  $("config-provider").addEventListener("change", () => {
    fillModelSuggestions();
    const first = $("config-models").querySelector("option")?.value;
    if (first) $("config-model").value = first;
  });
  $("theme-toggle").addEventListener("click", cycleTheme);
  $("help-toggle").addEventListener("click", () => $("help").showModal());
  $("help-close").addEventListener("click", () => $("help").close());
}

async function boot() {
  applyTheme(savedTheme());
  bind();
  renderInspector();
  try {
    await loadOverview();
  } catch (error) {
    showError(error);
  }
  const requested = new URLSearchParams(location.hash.slice(1)).get("run");
  const known = state.overview?.runs.some(({ run }) => run.id === requested);
  selectRun(known ? requested : newestRunId());
  openStream({
    onEvent,
    onResync: resync,
    onState: (connection) => {
      state.connection = connection;
      renderStatus();
    },
  });
}

boot();
