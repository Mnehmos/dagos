// Browser glue: holds UI state (project, conversation, view), loads recorded state from the API,
// follows the live event stream, and wires input. All DAGOS semantics stay in the runtime;
// rendering lives in chat.js / view.js and derivations in model.js.

import { api, openStream } from "./api.js";
import * as chat from "./chat.js";
import * as model from "./model.js";
import { knownModels, openSettings } from "./settings.js";
import * as view from "./view.js";

const $ = (id) => document.getElementById(id);
const TABS = view.TABS.map((tab) => tab.key);
const THEMES = ["system", "light", "dark"];

const state = {
  projects: [],
  projectId: null,
  overview: null,
  /** The open conversation; `null` is a new chat that starts with the next message. */
  conversationId: null,
  conversation: null,
  mode: "chat",
  /** The run shown in Inspect, and whose context the DAG panel highlights. */
  selectedRunId: null,
  detail: null,
  tab: "conversation",
  selectedNodeId: null,
  /** Streamed prose per running run, as it arrives: `{ [runId]: text }`. */
  live: {},
  connection: "connecting",
  configOpen: false,
  menuOpen: false,
  renaming: false,
  sending: false,
  bannerDismissed: null,
};

// ---------------------------------------------------------------------------------------------
// Location: #p=<project>&c=<conversation>&run=<run>&view=inspect

function readLocation() {
  const params = new URLSearchParams(location.hash.slice(1));
  return { project: params.get("p"), conversation: params.get("c"), run: params.get("run"), mode: params.get("view") };
}

function writeLocation() {
  const params = new URLSearchParams();
  if (state.projectId) params.set("p", state.projectId);
  if (state.conversationId) params.set("c", state.conversationId);
  if (state.mode === "inspect") {
    params.set("view", "inspect");
    if (state.selectedRunId) params.set("run", state.selectedRunId);
  }
  history.replaceState(null, "", `#${params}`);
}

// ---------------------------------------------------------------------------------------------
// Loading

async function loadProjects() {
  const { projects, default: fallback } = await api.projects();
  state.projects = projects;
  if (!projects.some(({ project }) => project.id === state.projectId)) state.projectId = fallback;
}

async function loadOverview() {
  state.overview = await api.projectOverview(state.projectId);
  renderChrome();
  renderConversations();
  renderDag();
  renderStatus();
}

async function loadConversation() {
  if (!state.conversationId) {
    state.conversation = null;
    renderChat();
    return;
  }
  const id = state.conversationId;
  const conversation = await api.conversation(id);
  if (id !== state.conversationId) return;
  state.conversation = conversation;
  for (const turn of conversation.turns) {
    if (turn.run.status !== "running") delete state.live[turn.run.id];
  }
  renderChat();
}

/** Loads the run the inspector and DAG panel follow: the selected one, or the chat's latest. */
async function loadDetail() {
  const runId = followedRunId();
  if (!runId) {
    state.detail = null;
    renderInspector();
    renderDag();
    return;
  }
  const detail = await api.run(runId);
  if (runId !== followedRunId()) return;
  state.detail = detail;
  renderInspector({ keepScroll: true });
  renderDag();
}

function turns() {
  return state.conversation?.turns ?? [];
}

function followedRunId() {
  if (state.mode === "inspect" && state.selectedRunId) return state.selectedRunId;
  return turns().at(-1)?.run.id ?? null;
}

let refreshTimer = null;
function scheduleRefresh() {
  clearTimeout(refreshTimer);
  refreshTimer = setTimeout(() => refresh().catch(showError), 120);
}

async function refresh() {
  await Promise.all([loadOverview(), loadConversation()]);
  await loadDetail();
}

// ---------------------------------------------------------------------------------------------
// Live events

function onEvent(event) {
  // Each inference step streams afresh; validated prose is shown from the recorded turn.
  if (event.type === "inference.started" || event.type === "response.validated") state.live[event.run_id] = "";
  if (event.type === "inference.delta") {
    state.live[event.run_id] = (state.live[event.run_id] ?? "") + event.payload.text;
    if (!showLiveProse(event.run_id)) scheduleRefresh();
    return;
  }
  scheduleRefresh();
}

/** Updates a streaming reply in place; false if it is not on screen. */
function showLiveProse(runId) {
  const inspectorProse = $("live-prose");
  if (inspectorProse && state.mode === "inspect" && state.detail?.run.id === runId) {
    inspectorProse.textContent = state.live[runId];
    return true;
  }
  const target = document.querySelector(`[data-live-run="${CSS.escape(runId)}"]`);
  if (!target || target.classList.contains("typing")) return false;
  const pinned = isPinned();
  target.textContent = state.live[runId];
  if (pinned) scrollToBottom();
  return true;
}

function resync() {
  refresh().catch(showError);
}

// ---------------------------------------------------------------------------------------------
// Rendering

function renderChrome() {
  const overview = state.overview;
  if (!overview) return;
  $("project-name").textContent = overview.project.name;
  const defaults = overview.run_defaults;
  $("config-provider-chip").textContent = defaults.provider_id;
  $("config-model-chip").textContent = defaults.model_id;
  if (!state.configOpen) fillConfigForm();
  if (state.menuOpen) renderProjectMenu();
  renderTitle();

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

function renderTitle() {
  if (!state.overview) return;
  document.title = `${state.conversation?.conversation.title ?? "New chat"} · ${state.overview.project.name} · DAGOS`;
}

function renderConversations() {
  if (!state.overview) return;
  const open = $("conversations").querySelector("details.archived")?.open ?? false;
  $("conversations").innerHTML = chat.conversationListHtml(state.overview.conversations, state.conversationId, {
    showArchived: open || Boolean(state.conversation?.conversation.archived_at),
  });
}

function isPinned() {
  const scroller = $("thread-scroll");
  return scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight < 80;
}

function scrollToBottom() {
  const scroller = $("thread-scroll");
  scroller.scrollTop = scroller.scrollHeight;
}

function renderChat({ forceScroll = false } = {}) {
  const pinned = forceScroll || isPinned();
  const opened = [...$("thread").querySelectorAll("[data-card] > details[open]")].map((details) => details.parentElement.dataset.card);
  $("chat-header").innerHTML = chat.chatHeaderHtml(state.conversation?.conversation, { renaming: state.renaming });
  $("thread").innerHTML = state.conversation?.turns.length
    ? chat.threadHtml(state.conversation, { live: state.live })
    : chat.newChatHtml(state.overview);
  for (const card of opened) {
    const details = $("thread").querySelector(`[data-card="${CSS.escape(card)}"] > details`);
    if (details) details.open = true;
  }
  if (pinned) scrollToBottom();
  if (state.renaming) $("chat-header").querySelector("input")?.select();
  renderTitle();
  renderInspectBar();
}

function renderMode() {
  const inspecting = state.mode === "inspect";
  $("chat").hidden = inspecting;
  $("inspect").hidden = !inspecting;
  $("mode-chat").setAttribute("aria-selected", String(!inspecting));
  $("mode-inspect").setAttribute("aria-selected", String(inspecting));
  renderInspectBar();
  renderInspector();
}

function renderInspectBar() {
  if (state.mode !== "inspect") return;
  const all = turns();
  const index = all.findIndex((turn) => turn.run.id === followedRunId());
  const title = state.conversation?.conversation.title ?? "New chat";
  $("inspect-bar").innerHTML = `<button type="button" class="back" data-action="to-chat">← ${view.esc(title)}</button>
    ${
      all.length
        ? `<span class="turn-nav"><button type="button" class="icon-button" data-action="prev-turn" aria-label="Previous turn" ${index <= 0 ? "disabled" : ""}>‹</button>
           <span>Turn ${index + 1} of ${all.length}</span>
           <button type="button" class="icon-button" data-action="next-turn" aria-label="Next turn" ${index >= all.length - 1 ? "disabled" : ""}>›</button></span>`
        : ""
    }`;
}

function renderInspector({ keepScroll = false } = {}) {
  if (state.mode !== "inspect") return;
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
  if (!followedRunId()) {
    root.innerHTML = view.emptyInspectorHtml(state.overview);
    return;
  }
  if (!state.detail || state.detail.run.id !== followedRunId()) {
    root.innerHTML = `<p class="loading">Loading run…</p>`;
    return;
  }
  const recorded = state.detail.streamed ?? "";
  const live = state.live[state.detail.run.id] ?? "";
  root.innerHTML = view.inspectorHtml({
    detail: state.detail,
    dag: state.overview.dag,
    tab: state.tab,
    streamed: live.length > recorded.length ? live : recorded,
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
    runId: state.detail?.run.id ?? null,
    memberSources: new Map(members.map((member) => [member.node_id, member.source])),
  });
}

function renderStatus() {
  $("status").innerHTML = view.statusHtml(state.overview, state.connection);
  const indicator = $("connection");
  indicator.dataset.state = state.connection;
  indicator.textContent = { live: "Live", connecting: "Connecting…", offline: "Reconnecting…" }[state.connection];
}

function renderProjectMenu() {
  $("project-menu").innerHTML = chat.projectMenuHtml(state.projects, state.projectId);
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
// Navigation

async function selectProject(projectId) {
  toggleMenu(false);
  if (projectId === state.projectId) return;
  state.projectId = projectId;
  state.conversationId = null;
  state.conversation = null;
  state.selectedRunId = null;
  state.detail = null;
  state.selectedNodeId = null;
  await loadOverview();
  state.conversationId = state.overview.conversations.find((c) => !c.archived_at)?.id ?? null;
  writeLocation();
  await loadConversation();
  renderChat({ forceScroll: true });
  renderConversations();
  await loadDetail();
}

async function selectConversation(conversationId) {
  state.conversationId = conversationId;
  state.renaming = false;
  state.selectedRunId = null;
  state.selectedNodeId = null;
  state.detail = null;
  renderConversations();
  await loadConversation();
  renderChat({ forceScroll: true });
  if (state.mode === "inspect") state.selectedRunId = turns().at(-1)?.run.id ?? null;
  writeLocation();
  await loadDetail();
}

function newChat() {
  state.conversationId = null;
  state.conversation = null;
  state.renaming = false;
  state.detail = null;
  state.selectedRunId = null;
  setMode("chat");
  renderConversations();
  renderChat();
  renderDag();
  writeLocation();
  $("message").focus();
}

function setMode(mode) {
  state.mode = mode;
  if (mode === "inspect" && !state.selectedRunId) state.selectedRunId = turns().at(-1)?.run.id ?? null;
  renderMode();
  writeLocation();
  loadDetail().catch(showError);
}

function inspectRun(runId) {
  state.selectedRunId = runId;
  state.tab = "conversation";
  state.detail = null;
  setMode("inspect");
}

function moveTurn(step) {
  const all = turns();
  if (!all.length) return;
  const index = all.findIndex((turn) => turn.run.id === followedRunId());
  const next = all[Math.min(all.length - 1, Math.max(0, (index === -1 ? all.length - 1 : index) + step))];
  inspectRun(next.run.id);
}

function moveConversation(step) {
  const list = (state.overview?.conversations ?? []).filter((c) => !c.archived_at);
  if (!list.length) return;
  const index = list.findIndex((c) => c.id === state.conversationId);
  const next = list[Math.min(list.length - 1, Math.max(0, (index === -1 ? 0 : index) + step))];
  selectConversation(next.id)
    .then(() => $("conversations").querySelector(`[data-conversation="${next.id}"]`)?.focus())
    .catch(showError);
}

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

// ---------------------------------------------------------------------------------------------
// Actions

async function sendMessage(event) {
  event.preventDefault();
  if (state.sending) return;
  const input = $("message");
  const message = input.value.trim();
  if (!message) return;
  state.sending = true;
  const button = $("send");
  button.disabled = true;
  try {
    const body = state.conversationId
      ? { message, conversation_id: state.conversationId }
      : { message, project_id: state.projectId, new_conversation: true };
    const { run } = await api.start(body);
    input.value = "";
    autosize();
    state.conversationId = run.conversation_id;
    state.renaming = false;
    if (state.mode === "inspect") state.selectedRunId = run.id;
    writeLocation();
    await Promise.all([loadOverview(), loadConversation()]);
    renderChat({ forceScroll: true });
    await loadDetail();
  } catch (error) {
    showError(error);
  } finally {
    state.sending = false;
    button.disabled = false;
  }
}

async function answerTool(button) {
  const { approve, run, call } = button.dataset;
  const card = button.closest(".approval");
  for (const other of card?.querySelectorAll("button") ?? []) other.disabled = true;
  try {
    await api.answerTool(run, call, { decision: approve === "deny" ? "deny" : "allow", remember: approve === "always" });
    if (approve === "always") toast("Allowed. This tool now runs without asking; change it in Settings → Tools.");
    scheduleRefresh();
  } catch (error) {
    showError(error);
    scheduleRefresh();
  }
}

async function renameConversation(title) {
  try {
    await api.updateConversation(state.conversationId, { title });
    state.renaming = false;
    await Promise.all([loadOverview(), loadConversation()]);
  } catch (error) {
    showError(error);
  }
}

async function archiveConversation(archived) {
  try {
    await api.updateConversation(state.conversationId, { archived });
    toast(archived ? "Chat archived. Its runs stay durable; restore it any time." : "Chat restored.");
    if (archived) {
      await loadOverview();
      newChat();
    } else {
      await Promise.all([loadOverview(), loadConversation()]);
    }
  } catch (error) {
    showError(error);
  }
}

async function createProject(name) {
  try {
    const { project } = await api.createProject(name);
    await loadProjects();
    toast(`Created ${project.name}.`);
    await selectProject(project.id);
  } catch (error) {
    showError(error);
  }
}

async function renameProject(name) {
  try {
    await api.renameProject(state.projectId, name);
    await loadProjects();
    await loadOverview();
    toggleMenu(false);
  } catch (error) {
    showError(error);
  }
}

async function recover() {
  try {
    const { recovered } = await api.recover();
    hideBanner();
    toast(`Marked ${recovered.length} run(s) as interrupted.`);
    await refresh();
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

function toggleMenu(open = !state.menuOpen) {
  state.menuOpen = open;
  $("project-menu").hidden = !open;
  $("project-toggle").setAttribute("aria-expanded", String(open));
  if (!open) return;
  renderProjectMenu();
  $("project-menu").querySelector(".menu-item.selected, .menu-item")?.focus();
  api
    .projects()
    .then(({ projects }) => {
      state.projects = projects;
      if (state.menuOpen) renderProjectMenu();
    })
    .catch(showError);
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
  const id = $("config-provider").value;
  const provider = state.overview?.providers.find((candidate) => candidate.id === id);
  $("config-models").innerHTML = [...new Set([...(provider?.suggested_models ?? []), ...knownModels(id)])]
    .map((modelId) => `<option value="${view.esc(modelId)}"></option>`)
    .join("");
}

function toggleConfig(open = !state.configOpen) {
  state.configOpen = open;
  $("config-panel").hidden = !open;
  $("config-toggle").setAttribute("aria-expanded", String(open));
  if (open) {
    if (state.mode !== "chat") setMode("chat");
    fillConfigForm();
    $("config-provider").focus();
  }
}

async function saveConfig(event) {
  event.preventDefault();
  try {
    await api.saveProjectConfig(state.projectId, {
      provider_id: $("config-provider").value,
      model_id: $("config-model").value.trim(),
      system_prompt: $("config-prompt").value,
    });
    toggleConfig(false);
    await loadOverview();
    toast("Saved. This project's next runs use it.");
    $("message").focus();
  } catch (error) {
    showError(error);
  }
}

function showSettings(section = null) {
  if (state.configOpen) toggleConfig(false);
  openSettings({
    section,
    defaults: state.overview?.run_defaults,
    saveDefaults: (body) => api.saveProjectConfig(state.projectId, body),
    onChange: () => loadOverview().catch(showError),
    notify: (message, { error = false } = {}) => (error ? showError(new Error(message)) : toast(message)),
  });
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

function autosize() {
  const input = $("message");
  input.style.height = "auto";
  input.style.height = `${Math.min(input.scrollHeight, 240)}px`;
}

function onClick(event) {
  if (state.menuOpen && !event.target.closest(".project-switch")) toggleMenu(false);
  const target = event.target.closest(
    "[data-approve], [data-conversation], [data-project], [data-inspect], [data-tab], [data-node], [data-copy], [data-close-node], [data-action]",
  );
  if (!target || target.closest("#settings")) return;
  const data = target.dataset;
  if (data.approve) answerTool(target);
  else if (data.conversation) selectConversation(data.conversation).catch(showError);
  else if (data.project) selectProject(data.project).catch(showError);
  else if (data.inspect) inspectRun(data.inspect);
  else if (data.tab) setTab(data.tab);
  else if (data.node) selectNode(data.node === state.selectedNodeId ? null : data.node);
  else if (data.copy === "ir") copyIr();
  else if ("closeNode" in data) selectNode(null);
  else {
    const actions = {
      recover,
      settings: () => showSettings(data.section || null),
      rename: () => {
        state.renaming = true;
        renderChat();
      },
      "cancel-rename": () => {
        state.renaming = false;
        renderChat();
      },
      archive: () => archiveConversation(true),
      restore: () => archiveConversation(false),
      "to-chat": () => setMode("chat"),
      "prev-turn": () => moveTurn(-1),
      "next-turn": () => moveTurn(1),
      dismiss: () => {
        state.bannerDismissed = $("banner").dataset.kind === "warn" ? "stale" : state.bannerDismissed;
        hideBanner();
      },
    };
    actions[data.action]?.();
  }
}

function onSubmit(event) {
  const form = event.target.closest("[data-form]");
  if (!form) return;
  event.preventDefault();
  const field = form.dataset.form === "rename-conversation" ? "title" : "name";
  const value = String(new FormData(form).get(field) ?? "").trim();
  if (!value) return;
  if (form.dataset.form === "rename-conversation") renameConversation(value);
  else if (form.dataset.form === "new-project") createProject(value);
  else if (form.dataset.form === "rename-project") renameProject(value);
}

function onKeyDown(event) {
  if (event.key === "Escape") {
    if ($("settings").open || $("help").open) return;
    if (state.menuOpen) toggleMenu(false);
    else if (state.renaming) {
      state.renaming = false;
      renderChat();
    } else if (state.configOpen) toggleConfig(false);
    else if (state.selectedNodeId) selectNode(null);
    return;
  }
  const tab = event.target.closest?.(".tabs [role=tab]");
  if (tab && (event.key === "ArrowRight" || event.key === "ArrowLeft")) {
    event.preventDefault();
    const index = TABS.indexOf(tab.dataset.tab) + (event.key === "ArrowRight" ? 1 : -1);
    setTab(TABS[(index + TABS.length) % TABS.length], { focus: true });
    return;
  }
  if ($("settings").open || $("help").open) return;
  const typing = event.target.closest?.("input, textarea, select, [contenteditable]");
  if (typing || event.metaKey || event.ctrlKey || event.altKey) return;
  if (state.mode === "inspect" && /^[1-6]$/.test(event.key)) {
    setTab(TABS[Number(event.key) - 1]);
    return;
  }
  const actions = {
    "/": () => $("message").focus(),
    n: () => newChat(),
    j: () => (state.mode === "inspect" ? moveTurn(1) : moveConversation(1)),
    k: () => (state.mode === "inspect" ? moveTurn(-1) : moveConversation(-1)),
    v: () => setMode(state.mode === "chat" ? "inspect" : "chat"),
    p: () => toggleMenu(),
    c: () => toggleConfig(),
    s: () => showSettings(),
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
  document.addEventListener("submit", onSubmit);
  document.addEventListener("keydown", onKeyDown);
  $("composer").addEventListener("submit", sendMessage);
  $("message").addEventListener("input", autosize);
  $("message").addEventListener("keydown", (event) => {
    if (event.key === "Enter" && !event.shiftKey && !event.isComposing) {
      event.preventDefault();
      $("composer").requestSubmit();
    }
  });
  $("new-chat").addEventListener("click", newChat);
  $("mode-chat").addEventListener("click", () => setMode("chat"));
  $("mode-inspect").addEventListener("click", () => setMode("inspect"));
  $("project-toggle").addEventListener("click", (event) => {
    event.stopPropagation();
    toggleMenu();
  });
  $("config-toggle").addEventListener("click", () => toggleConfig());
  $("config-cancel").addEventListener("click", () => toggleConfig(false));
  $("config-form").addEventListener("submit", saveConfig);
  $("config-provider").addEventListener("change", () => {
    fillModelSuggestions();
    const first = $("config-models").querySelector("option")?.value;
    if (first) $("config-model").value = first;
  });
  $("config-manage").addEventListener("click", () => showSettings());
  $("settings-toggle").addEventListener("click", () => showSettings());
  $("theme-toggle").addEventListener("click", cycleTheme);
  $("help-toggle").addEventListener("click", () => $("help").showModal());
  $("help-close").addEventListener("click", () => $("help").close());
}

async function boot() {
  applyTheme(savedTheme());
  bind();
  const requested = readLocation();
  state.projectId = requested.project;
  try {
    await loadProjects();
    await loadOverview();
    const conversations = state.overview.conversations;
    state.conversationId = conversations.some((c) => c.id === requested.conversation)
      ? requested.conversation
      : (conversations.find((c) => !c.archived_at)?.id ?? null);
    state.mode = requested.mode === "inspect" ? "inspect" : "chat";
    await loadConversation();
    if (state.mode === "inspect") {
      state.selectedRunId = turns().some((turn) => turn.run.id === requested.run) ? requested.run : null;
    }
    renderConversations();
    renderMode();
    renderChat({ forceScroll: true });
    writeLocation();
    await loadDetail();
  } catch (error) {
    showError(error);
  }
  openStream({
    onEvent,
    onResync: resync,
    onState: (connection) => {
      state.connection = connection;
      renderStatus();
    },
  });
  $("message").focus();
}

boot();
