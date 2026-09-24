// Settings: providers, API keys, and the Jev. Rendering functions are pure (settings in, escaped
// HTML out) so they are tested under Node; `openSettings` wires them to the dialog. Keys typed
// here go straight to the local server, which stores them for this user; they are never shown
// again, only a short hint such as `sk-o…7890`.

import { api } from "./api.js";
import { esc } from "./view.js";

const SOURCE_TEXT = {
  env: "from the environment",
  saved: "saved in DAGOS",
  none: "no key",
  not_needed: "no key needed",
};

/** TypeSafe's Jev, a calibrated decisions model on OpenRouter, built for exactly this job. */
export const TYPESAFE_JEV = "~typesafe/jev-latest";

/** Model suggestions for Jev on `providerId`: TypeSafe's Jev first on OpenRouter. */
export function jevModels(provider, fetched = []) {
  const models = [...(provider?.models ?? []), ...fetched];
  return [...new Set(provider?.id === "openrouter" ? [TYPESAFE_JEV, ...models] : models)];
}

/** The status a provider shows in the list: `ready`, `needs-key`, or `keyless`. */
export function providerStatus(provider) {
  if (provider.origin === "builtin") return { kind: "ready", text: "Offline" };
  if (provider.key.source === "env" || provider.key.source === "saved") return { kind: "ready", text: "Ready" };
  if (provider.origin === "custom") return { kind: "keyless", text: "No key" };
  return { kind: "needs-key", text: "Needs key" };
}

/** The navigation list: every provider, then the Jev, then "add endpoint". */
export function navHtml(settings, selected) {
  const providers = settings.providers
    .map((provider) => {
      const status = providerStatus(provider);
      return `<li><button type="button" class="settings-nav-item${selected === provider.id ? " selected" : ""}" data-settings-select="${esc(provider.id)}" aria-current="${selected === provider.id}">
        <span class="settings-nav-name">${esc(provider.name)}</span>
        <span class="status-pill status-${status.kind}">${esc(status.text)}</span>
      </button></li>`;
    })
    .join("");
  const jev = settings.jev;
  const jevStatus = jev.problem
    ? { kind: "needs-key", text: "Fallback" }
    : jev.provider
      ? { kind: "ready", text: "Model" }
      : { kind: "keyless", text: "Offline" };
  return `<p class="section-label">Providers</p>
    <ul class="settings-nav-list">${providers}</ul>
    <button type="button" class="settings-nav-add${selected === "new" ? " selected" : ""}" data-settings-select="new">+ Add endpoint</button>
    <p class="section-label">Context</p>
    <ul class="settings-nav-list"><li><button type="button" class="settings-nav-item${selected === "jev" ? " selected" : ""}" data-settings-select="jev" aria-current="${selected === "jev"}">
      <span class="settings-nav-name">Jev</span>
      <span class="status-pill status-${jevStatus.kind}">${esc(jevStatus.text)}</span>
    </button></li></ul>`;
}

function keyHtml(provider, { replacing, keysFile }) {
  const key = provider.key;
  const manage = key.manage_url
    ? `<a href="${esc(key.manage_url)}" target="_blank" rel="noopener noreferrer">Get a ${esc(provider.name)} key ↗</a>`
    : "";
  const where = keysFile
    ? `Saved for your user in <code>${esc(keysFile)}</code> — never in the project, the database, or the IR.`
    : "No user configuration directory was found, so keys cannot be saved; set an environment variable instead.";
  const form = `<form class="key-form" data-settings-form="key" data-provider="${esc(provider.id)}" autocomplete="off">
      <label for="key-input">API key</label>
      <div class="key-row">
        <input id="key-input" name="key" type="password" autocomplete="off" spellcheck="false" placeholder="Paste your ${esc(provider.name)} API key" required ${keysFile ? "" : "disabled"}>
        <button type="button" class="icon-button" data-settings-action="reveal" aria-label="Show or hide the key" title="Show or hide">👁</button>
        <button type="submit" class="primary" ${keysFile ? "" : "disabled"}>Save &amp; test</button>
        ${replacing ? `<button type="button" data-settings-action="cancel-replace">Cancel</button>` : ""}
      </div>
      <p class="hint">${where} ${manage}</p>
    </form>`;

  if (key.source === "env") {
    return `<div class="key-state key-env">
        <p><span class="key-dot"></span>Using <code>${esc(key.env_var)}</code> from the environment <code class="key-hint">${esc(key.hint)}</code></p>
        <p class="hint">The environment always wins over a saved key. ${key.saved ? `A saved key is also stored. <button type="button" class="link" data-settings-action="remove-key">Remove saved key</button>` : ""}</p>
      </div>`;
  }
  if (key.source === "saved" && !replacing) {
    return `<div class="key-state key-saved">
        <p><span class="key-dot"></span>Key saved <code class="key-hint">${esc(key.hint)}</code></p>
        <div class="button-row start">
          <button type="button" data-settings-action="replace-key">Replace</button>
          <button type="button" class="danger" data-settings-action="remove-key">Remove</button>
        </div>
        ${key.env_var ? `<p class="hint">Setting <code>${esc(key.env_var)}</code> in the environment would take precedence.</p>` : ""}
      </div>`;
  }
  const keyless =
    provider.origin === "custom" && !replacing
      ? `<p class="hint">This endpoint is called without a key${key.env_var ? ` (<code>${esc(key.env_var)}</code> is not set)` : ""}. Local servers such as Ollama need none.</p>`
      : "";
  return `${keyless}${form}`;
}

function checkHtml(provider, check) {
  const usable = provider.available && provider.origin !== "builtin";
  if (!usable) return "";
  let result = "";
  if (check?.state === "checking") result = `<p class="check-result checking" role="status">Checking the connection…</p>`;
  else if (check?.state === "ok") {
    result = `<p class="check-result ok" role="status">✓ Connected${check.models.length ? ` · ${check.models.length} model(s) available` : ""}</p>`;
  } else if (check?.state === "error") {
    result = `<p class="check-result error" role="alert">✗ ${esc(check.message)}</p>`;
  }
  return `<div class="settings-block">
      <h4>Connection</h4>
      <div class="button-row start"><button type="button" data-settings-action="check">Test connection</button></div>
      ${result}
    </div>`;
}

function useHtml(provider, { check, defaults }) {
  if (!provider.available) return "";
  const models = [...new Set([...(provider.models ?? []), ...(check?.models ?? [])])];
  const current = defaults?.provider_id === provider.id ? defaults.model_id : (models[0] ?? "");
  const isDefault = defaults?.provider_id === provider.id;
  return `<form class="settings-block use-form" data-settings-form="use" data-provider="${esc(provider.id)}">
      <h4>Use for runs</h4>
      <label for="use-model">Model</label>
      <div class="key-row">
        <input id="use-model" name="model" list="use-models" value="${esc(current)}" autocomplete="off" spellcheck="false" placeholder="${provider.origin === "preset" ? "e.g. vendor/model-name — test the connection to list models" : "model id"}" required>
        <datalist id="use-models">${models.map((model) => `<option value="${esc(model)}"></option>`).join("")}</datalist>
        <button type="submit">${isDefault ? "Update" : "Use for new runs"}</button>
      </div>
      ${isDefault ? `<p class="hint">New runs use <code>${esc(defaults.provider_id)} / ${esc(defaults.model_id)}</code>.</p>` : ""}
    </form>`;
}

function customHtml(provider) {
  const models = (provider?.models ?? []).join("\n");
  const creating = !provider;
  return `<form class="settings-block endpoint-form" data-settings-form="endpoint" ${creating ? "" : `data-provider="${esc(provider.id)}"`}>
      <h4>${creating ? "OpenAI-compatible endpoint" : "Endpoint"}</h4>
      ${creating ? `<label for="endpoint-id">ID</label><input id="endpoint-id" name="id" required pattern="[a-z0-9][a-z0-9._\\-]{0,63}" placeholder="e.g. ollama" spellcheck="false" autocomplete="off"><p class="hint">Lowercase letters, digits, <code>.</code> <code>_</code> <code>-</code>. Runs record it.</p>` : ""}
      <label for="endpoint-url">Base URL</label>
      <input id="endpoint-url" name="base_url" type="url" required value="${esc(provider?.base_url ?? "")}" placeholder="http://localhost:11434/v1" spellcheck="false">
      <label for="endpoint-env">Key environment variable <span class="optional">optional</span></label>
      <input id="endpoint-env" name="api_key_env" value="${esc(provider?.key.env_var ?? "")}" placeholder="e.g. TOGETHER_API_KEY" spellcheck="false" autocomplete="off">
      <label for="endpoint-models">Models <span class="optional">one per line</span></label>
      <textarea id="endpoint-models" name="models" rows="3" spellcheck="false">${esc(models)}</textarea>
      <label class="check"><input type="checkbox" name="json_mode" ${provider?.json_mode === false ? "" : "checked"}> Request JSON output (<code>response_format</code>)</label>
      <div class="button-row start">
        <button type="submit" class="primary">${creating ? "Add endpoint" : "Save endpoint"}</button>
        ${creating ? "" : `<button type="button" class="danger" data-settings-action="remove-provider">Remove endpoint</button>`}
      </div>
    </form>`;
}

/** The detail pane for one provider. */
export function providerHtml(provider, context) {
  const status = providerStatus(provider);
  const facts = provider.base_url
    ? `<dl class="facts compact"><div><dt>ID</dt><dd><code>${esc(provider.id)}</code></dd></div><div><dt>Endpoint</dt><dd><code>${esc(provider.base_url)}</code></dd></div></dl>`
    : `<dl class="facts compact"><div><dt>ID</dt><dd><code>${esc(provider.id)}</code></dd></div></dl>`;
  if (provider.origin === "builtin") {
    return `<header class="settings-head"><h3>${esc(provider.name)}</h3><span class="status-pill status-${status.kind}">${esc(status.text)}</span></header>
      ${facts}
      <p class="callout">Deterministic and offline: no network, no key. Models such as <code>fake-malformed</code>, <code>fake-cycle</code>, and <code>fake-timeout</code> show that failures stay explicit.</p>
      ${useHtml(provider, context)}`;
  }
  return `<header class="settings-head"><h3>${esc(provider.name)}</h3><span class="status-pill status-${status.kind}">${esc(status.text)}</span></header>
    ${facts}
    <div class="settings-block"><h4>API key <span class="optional">${esc(SOURCE_TEXT[provider.key.source])}</span></h4>${keyHtml(provider, context)}</div>
    ${checkHtml(provider, context.check)}
    ${useHtml(provider, context)}
    ${provider.origin === "custom" ? customHtml(provider) : ""}`;
}

/** The Jev pane: offline policy or a model, with the offline policy as the fallback. */
export function jevHtml(settings, { checks }) {
  const jev = settings.jev;
  const candidates = settings.providers.filter((provider) => provider.origin !== "builtin");
  const selected = jev.provider ?? candidates.find((provider) => provider.available)?.id ?? candidates[0]?.id;
  const selectedProvider = candidates.find((provider) => provider.id === selected);
  const models = jevModels(selectedProvider, checks[selected]?.models);
  const locked = jev.source === "env";
  const usingModel = Boolean(jev.provider);
  return `<header class="settings-head"><h3>Jev</h3><span class="status-pill status-${jev.problem ? "needs-key" : usingModel ? "ready" : "keyless"}">${jev.problem ? "Fallback" : usingModel ? "Model" : "Offline"}</span></header>
    <p class="callout">Jev decides each run's <strong>active context</strong>: which durable nodes the model sees. Its classification is applied as the context — it cannot write to the DAG, answer the message, or choose providers. The offline policy always works; a model Jev judges relevance better.</p>
    <dl class="facts compact">
      <div><dt>Active</dt><dd><code>${esc(jev.active)}</code></dd></div>
      <div><dt>Fallback</dt><dd><code>${esc(jev.fallback)}</code> — used whenever the model Jev fails, times out, or returns anything but a valid classification; the run records why.</dd></div>
    </dl>
    ${jev.problem ? `<p class="callout callout-warn">${esc(jev.problem)}</p>` : ""}
    ${locked ? `<p class="callout">Set by <code>DAGOS_JEV_PROVIDER</code> and <code>DAGOS_JEV_MODEL</code> in the environment, which override this screen.</p>` : ""}
    <form class="settings-block jev-form" data-settings-form="jev">
      <fieldset ${locked ? "disabled" : ""}>
        <legend class="sr-only">Classifier</legend>
        <label class="choice"><input type="radio" name="mode" value="offline" ${usingModel ? "" : "checked"}> <span><strong>Offline policy</strong><br><span class="hint">Deterministic, free, no network.</span></span></label>
        <label class="choice"><input type="radio" name="mode" value="model" ${usingModel ? "checked" : ""} ${candidates.length ? "" : "disabled"}> <span><strong>Model</strong><br><span class="hint">A fast, inexpensive model is plenty; it only labels nodes.</span></span></label>
        <div class="jev-model">
          <label for="jev-provider">Provider</label>
          <select id="jev-provider" name="provider">${candidates
            .map((provider) => `<option value="${esc(provider.id)}" ${provider.id === selected ? "selected" : ""}>${esc(provider.name)}${provider.available ? "" : " (needs key)"}</option>`)
            .join("")}</select>
          <label for="jev-model">Model</label>
          <input id="jev-model" name="model" list="jev-models" value="${esc(jev.model ?? "")}" autocomplete="off" spellcheck="false" placeholder="${selected === "openrouter" ? TYPESAFE_JEV : "e.g. a small, fast model id"}">
          <p class="hint">On OpenRouter, <code>${TYPESAFE_JEV}</code> (TypeSafe's Jev) is recommended: it answers one calibrated yes/no per node through the Decisions API. Other models classify through chat.</p>
          <datalist id="jev-models">${models.map((model) => `<option value="${esc(model)}"></option>`).join("")}</datalist>
        </div>
        <div class="button-row start"><button type="submit" class="primary">Save</button></div>
      </fieldset>
    </form>`;
}

/** The pane for adding an endpoint. */
export function newEndpointHtml() {
  return `<header class="settings-head"><h3>Add endpoint</h3></header>
    <p class="callout">Any server that speaks OpenAI-compatible Chat Completions: Ollama, LM Studio, vLLM, Together, Groq, a company gateway. Add a key afterwards if it needs one.</p>
    ${customHtml(null)}`;
}

// ---------------------------------------------------------------------------------------------
// Tools (MCP servers)

const POLICIES = [
  { value: "off", label: "Off", hint: "Not offered to models" },
  { value: "ask", label: "Ask", hint: "You approve each call in the chat" },
  { value: "allow", label: "Allow", hint: "Runs without asking" },
];

/** A server's status pill: off, error, or how many of its tools models are offered. */
export function toolServerStatus(status) {
  if (!status || status.enabled === false) return { kind: "keyless", text: "Off" };
  if (status.error) return { kind: "needs-key", text: "Error" };
  const offered = status.tools.filter((tool) => tool.policy !== "off").length;
  return { kind: "ready", text: `${offered}/${status.tools.length} tools` };
}

/** The Tools part of the settings navigation. */
export function toolsNavHtml(tools, selected) {
  const servers = tools?.config?.servers ?? [];
  const statuses = tools?.capabilities?.servers ?? [];
  const items = servers
    .map((server) => {
      const key = `tool:${server.id}`;
      const status = toolServerStatus(statuses.find((candidate) => candidate.id === server.id) ?? { enabled: server.enabled !== false, tools: [], error: "not started" });
      return `<li><button type="button" class="settings-nav-item${selected === key ? " selected" : ""}" data-settings-select="${esc(key)}" aria-current="${selected === key}">
        <span class="settings-nav-name">${esc(server.id)}</span>
        <span class="status-pill status-${status.kind}">${esc(status.text)}</span>
      </button></li>`;
    })
    .join("");
  return `<p class="section-label">Tools</p>
    ${items ? `<ul class="settings-nav-list">${items}</ul>` : ""}
    <button type="button" class="settings-nav-add${selected === "tool:new" ? " selected" : ""}" data-settings-select="tool:new">+ Add MCP server</button>`;
}

/** The review loop's and the tool guard's entries in the settings list. */
export function safetyNavHtml(lint, tools, selected) {
  const item = (key, name, on) => `<li><button type="button" class="settings-nav-item${selected === key ? " selected" : ""}" data-settings-select="${key}" aria-current="${selected === key}">
      <span class="settings-nav-name">${name}</span>
      <span class="status-pill status-${on ? "ready" : "keyless"}">${on ? "On" : "Off"}</span>
    </button></li>`;
  const review = lint?.config.enabled ?? true;
  const guard = tools?.config?.guard?.enabled ?? true;
  return `<p class="section-label">Safety and quality</p>
    <ul class="settings-nav-list">${item("review", "Review loop", review)}${item("guard", "Tool guard", guard)}</ul>`;
}

/** A rule ID from its text, e.g. `swallows-errors` for "Swallows errors." */
export function ruleId(text) {
  return text.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "").slice(0, 48);
}

/** The review loop's pane: on/off, threshold, review limit, and the rules (`rules` is the draft). */
export function reviewHtml(lint, rules) {
  if (!lint) return `<p class="loading">Loading…</p>`;
  const config = lint.config;
  const rows = rules
    .map(
      (rule, index) => `<li class="rule-row">
        <div class="rule-main">
          <input name="rule_text" value="${esc(rule.text)}" placeholder="A problem, in a few words, e.g. Swallows errors." aria-label="Rule ${index + 1}" required>
          <button type="button" class="link danger" data-settings-action="rule-remove" data-index="${index}" aria-label="Remove rule ${index + 1}">Remove</button>
        </div>
        <input type="hidden" name="rule_id" value="${esc(rule.id)}">
        <details class="rule-hints">
          <summary>Hints for the judge</summary>
          <label>Applies when<textarea name="rule_applies" rows="2" placeholder="What counts, in more detail.">${esc(rule.applies ?? "")}</textarea></label>
          <label>Not a problem when<textarea name="rule_except" rows="2" placeholder="Accepted exceptions in this project.">${esc(rule.except ?? "")}</textarea></label>
        </details>
      </li>`,
    )
    .join("");
  return `<header class="settings-head"><h3>Review loop</h3><span class="status-pill status-${config.enabled ? "ready" : "keyless"}">${config.enabled ? "On" : "Off"}</span></header>
    <p class="callout">When a run's tool calls change code, Jev judges every changed function against these plain-English rules (one yes/no per rule). Rules it judges likely go back to the model, which fixes them or explains why they are wrong, before the run ends. <code>dagos lint</code> uses the same rules. Needs TypeSafe's Jev.</p>
    <form class="settings-block review-form" data-settings-form="review">
      <label class="choice"><input type="checkbox" name="enabled" ${config.enabled ? "checked" : ""}> <span><strong>Review the code runs change</strong></span></label>
      <div class="field-row">
        <label>Finding at <input type="number" name="threshold" min="0" max="1" step="0.05" value="${config.threshold}"> or above</label>
        <label>Reviews per run <input type="number" name="max_rounds" min="1" max="10" step="1" value="${config.max_rounds}"></label>
      </div>
      <p class="section-label">Rules (${rules.length})</p>
      <ol class="rule-list">${rows}</ol>
      <div class="button-row start">
        <button type="button" data-settings-action="rule-add">+ Add rule</button>
        <button type="button" class="link" data-settings-action="rules-reset">Restore the default rules</button>
      </div>
      <p class="hint">Saved in <code>${esc(lint.file)}</code>.</p>
      <div class="button-row start"><button type="submit" class="primary">Save</button></div>
    </form>`;
}

/** The review form's values as a lint configuration. */
export function readReviewForm(form) {
  const data = new FormData(form);
  const all = (name) => data.getAll(name).map((value) => String(value));
  const [ids, texts, applies, excepts] = ["rule_id", "rule_text", "rule_applies", "rule_except"].map(all);
  const rules = texts.map((text, index) => ({
    id: ids[index] || ruleId(text) || `rule-${index + 1}`,
    text: text.trim(),
    applies: applies[index]?.trim() || null,
    except: excepts[index]?.trim() || null,
  }));
  return {
    enabled: data.get("enabled") === "on",
    threshold: Number(data.get("threshold")),
    max_rounds: Number(data.get("max_rounds")),
    rules,
  };
}

/** The tool guard's pane: on/off, threshold, and the risks it asks about. */
export function guardHtml(tools) {
  if (!tools) return `<p class="loading">Loading…</p>`;
  const guard = tools.config.guard ?? { enabled: true, threshold: 0.5 };
  const risks = (tools.guard_risks ?? [])
    .map((risk) => `<li><code>${esc(risk.id)}</code> ${esc(risk.meaning)}${risk.always_asks ? ` <span class="status-pill status-needs-key">always asks</span>` : ""}</li>`)
    .join("");
  return `<header class="settings-head"><h3>Tool guard</h3><span class="status-pill status-${guard.enabled ? "ready" : "keyless"}">${guard.enabled ? "On" : "Off"}</span></header>
    <p class="callout">Before a tool call runs, Jev judges it against these risks in the light of your request. A likely risk turns an <strong>Allow</strong> into a question for you, and the approval says why. The guard only ever asks more, never less; policies still decide first. Needs TypeSafe's Jev.</p>
    <form class="settings-block guard-form" data-settings-form="guard">
      <label class="choice"><input type="checkbox" name="enabled" ${guard.enabled ? "checked" : ""}> <span><strong>Check tool calls before they run</strong></span></label>
      <label>Ask at <input type="number" name="threshold" min="0" max="1" step="0.05" value="${guard.threshold}"> or above <span class="hint">(lower asks more often)</span></label>
      <div class="button-row start"><button type="submit" class="primary">Save</button></div>
    </form>
    <p class="section-label">Risks</p>
    <ul class="risk-list">${risks}</ul>
    <p class="hint">Risks marked <em>always asks</em> need you even when your request asked for them.</p>`;
}

function policyControlHtml(serverId, tool, current) {
  return `<span class="policy-control" role="group" aria-label="${esc(tool ?? "all tools")} policy">${POLICIES.map(
    (policy) => `<button type="button" class="policy-${policy.value}" data-settings-action="tool-policy" data-server="${esc(serverId)}"${tool ? ` data-tool="${esc(tool)}"` : ""} data-policy="${policy.value}" aria-pressed="${current === policy.value}" title="${esc(policy.hint)}">${policy.label}</button>`,
  ).join("")}</span>`;
}

function serverFormHtml(server) {
  const creating = !server;
  return `<form class="settings-block tool-server-form" data-settings-form="tool-server"${creating ? "" : ` data-server="${esc(server.id)}"`}>
      <h4>${creating ? "Stdio MCP server" : "Server"}</h4>
      ${creating ? `<label for="tool-id">ID</label><input id="tool-id" name="id" required pattern="[A-Za-z0-9_\-]+" placeholder="e.g. ooda" autocomplete="off" spellcheck="false"><p class="hint">Tools appear to models as <code>&lt;id&gt;.&lt;tool&gt;</code>.</p>` : ""}
      <label for="tool-command">Command</label>
      <input id="tool-command" name="command" required value="${esc(server?.command ?? "")}" placeholder="node, npx.cmd, uvx, python…" autocomplete="off" spellcheck="false">
      <label for="tool-args">Arguments <span class="optional">one per line</span></label>
      <textarea id="tool-args" name="args" rows="3" spellcheck="false">${esc((server?.args ?? []).join("\n"))}</textarea>
      <label for="tool-cwd">Working directory <span class="optional">optional</span></label>
      <input id="tool-cwd" name="cwd" value="${esc(server?.cwd ?? "")}" autocomplete="off" spellcheck="false">
      <label class="check"><input type="checkbox" name="enabled" ${server?.enabled === false ? "" : "checked"}> Enabled (started with DAGOS)</label>
      <div class="button-row start">
        <button type="submit" class="primary">${creating ? "Add and start" : "Save and restart"}</button>
        ${creating ? "" : `<button type="button" class="danger" data-settings-action="remove-tool-server" data-server="${esc(server.id)}">Remove server</button>`}
      </div>
    </form>`;
}

/** The pane for one MCP server: status, bulk and per-tool policies, and its launch settings. */
export function toolServerHtml(server, status, { filter = "" } = {}) {
  const pill = toolServerStatus(status ?? { enabled: server.enabled !== false, tools: [], error: "not started" });
  const tools = status?.tools ?? [];
  const offered = tools.filter((tool) => tool.policy !== "off").length;
  const needle = filter.trim().toLowerCase();
  const rows = tools
    .map((tool) => {
      const hidden = needle && !`${tool.name} ${tool.description}`.toLowerCase().includes(needle);
      return `<tr data-tool-row="${esc(`${tool.name} ${tool.description}`.toLowerCase())}"${hidden ? " hidden" : ""}>
        <td class="grow"><code>${esc(tool.name)}</code><div class="tool-description">${esc(tool.description)}</div></td>
        <td>${policyControlHtml(server.id, tool.name, tool.policy)}</td>
      </tr>`;
    })
    .join("");
  const body = status?.error
    ? `<p class="callout callout-error">${esc(status.error)}</p><p class="hint">Check the command and working directory below, then save to restart it.</p>`
    : server.enabled === false
      ? `<p class="callout">This server is disabled: it is not started and models see none of its tools.</p>`
      : tools.length
        ? `<div class="settings-block">
            <h4>Tools <span class="optional">${offered} of ${tools.length} offered to models</span></h4>
            <p class="hint">Every offered tool's description and schema go into each request, so offer only what you need. <strong>Ask</strong> shows Allow / Deny in the chat before a call runs; <strong>Allow</strong> runs it without asking.</p>
            <div class="bulk-row"><span>Set all:</span>${policyControlHtml(server.id, null, null)}
              <input class="tool-filter" type="search" placeholder="Filter ${tools.length} tools" value="${esc(filter)}" aria-label="Filter tools" data-settings-filter></div>
            <table class="grid tool-table"><tbody>${rows}</tbody></table>
          </div>`
        : `<p class="empty-note">The server describes no tools.</p>`;
  return `<header class="settings-head"><h3>${esc(server.id)}</h3><span class="status-pill status-${pill.kind}">${esc(pill.text)}</span></header>
    <dl class="facts compact"><div><dt>Command</dt><dd><code>${esc([server.command, ...(server.args ?? [])].join(" "))}</code></dd></div>${server.cwd ? `<div><dt>In</dt><dd><code>${esc(server.cwd)}</code></dd></div>` : ""}</dl>
    ${body}
    ${serverFormHtml(server)}`;
}

/** The pane for adding a server: import from Claude Desktop, or enter it by hand. */
export function newToolServerHtml(imports) {
  const candidates = imports?.servers ?? [];
  const list = candidates
    .map(
      (candidate, index) => `<li class="import-item">
        <div><strong>${esc(candidate.name)}</strong> <code>${esc([candidate.command, ...candidate.args].join(" "))}</code>
          ${candidate.needs_env ? `<p class="hint">Uses environment variables in Claude Desktop, which DAGOS does not copy; set them before starting DAGOS.</p>` : ""}</div>
        ${candidate.added ? `<span class="status-pill status-ready">Added</span>` : `<button type="button" data-settings-action="import-tool-server" data-index="${index}">Import</button>`}
      </li>`,
    )
    .join("");
  return `<header class="settings-head"><h3>Add MCP server</h3></header>
    <p class="callout">MCP servers give models tools. DAGOS starts each server, offers its tools to models, and runs a call only when the tool's policy allows it or you approve it. New tools start as <strong>Ask</strong>.</p>
    ${
      candidates.length
        ? `<div class="settings-block"><h4>From Claude Desktop</h4><ul class="import-list">${list}</ul><p class="hint">Only the command, arguments, and working directory are imported.</p></div>`
        : ""
    }
    ${serverFormHtml(null)}`;
}

// ---------------------------------------------------------------------------------------------
// Controller

const ui = {
  settings: null,
  selected: null,
  replacing: false,
  checks: {},
  defaults: null,
  onChange: () => {},
  notify: () => {},
  saveDefaults: (body) => api.saveConfig(body),
  tools: null,
  imports: null,
  toolFilter: "",
};

const $ = (id) => document.getElementById(id);

/** Models a connection check found for `providerId`, for other model pickers. */
export function knownModels(providerId) {
  return ui.checks[providerId]?.models ?? [];
}

function render() {
  const settings = ui.settings;
  if (!settings) {
    $("settings-detail").innerHTML = `<p class="loading">Loading…</p>`;
    return;
  }
  const toolServer = ui.selected?.startsWith("tool:") ? ui.selected.slice(5) : null;
  const knownTool = toolServer === "new" || (ui.tools?.config.servers ?? []).some((server) => server.id === toolServer);
  if (toolServer && !knownTool && ui.tools) ui.selected = "tool:new";
  const fixed = ["jev", "new", "review", "guard"];
  if (!ui.selected || (!ui.selected.startsWith("tool:") && !fixed.includes(ui.selected) && !settings.providers.some((p) => p.id === ui.selected))) {
    ui.selected = settings.providers.find((p) => p.origin !== "builtin" && !p.available)?.id ?? settings.providers[1]?.id ?? "fake";
  }
  $("settings-nav").innerHTML =
    navHtml(settings, ui.selected) + safetyNavHtml(ui.lint, ui.tools, ui.selected) + toolsNavHtml(ui.tools, ui.selected);
  const detail = $("settings-detail");
  if (ui.selected === "tool:new") detail.innerHTML = newToolServerHtml(ui.imports);
  else if (ui.selected.startsWith("tool:")) {
    const id = ui.selected.slice(5);
    const server = ui.tools?.config.servers.find((candidate) => candidate.id === id);
    const status = ui.tools?.capabilities?.servers.find((candidate) => candidate.id === id);
    detail.innerHTML = server ? toolServerHtml(server, status, { filter: ui.toolFilter }) : `<p class="loading">Loading…</p>`;
  } else if (ui.selected === "jev") detail.innerHTML = jevHtml(settings, { checks: ui.checks });
  else if (ui.selected === "review") {
    ui.lintDraft ??= structuredClone(ui.lint?.config.rules ?? []);
    detail.innerHTML = reviewHtml(ui.lint, ui.lintDraft);
  } else if (ui.selected === "guard") detail.innerHTML = guardHtml(ui.tools);
  else if (ui.selected === "new") detail.innerHTML = newEndpointHtml();
  else {
    const provider = settings.providers.find((p) => p.id === ui.selected);
    detail.innerHTML = providerHtml(provider, {
      replacing: ui.replacing,
      keysFile: settings.keys_file,
      check: ui.checks[provider.id],
      defaults: ui.defaults,
    });
  }
  syncJevForm();
}

function syncJevForm() {
  const form = document.querySelector("[data-settings-form=jev]");
  if (!form) return;
  const model = form.elements.mode.value === "model";
  form.querySelector(".jev-model").hidden = !model;
}

function select(id) {
  ui.selected = id;
  ui.replacing = false;
  render();
  const focusable = $("settings-detail").querySelector("input:not([type=radio]):not(:disabled), select, button");
  focusable?.focus();
}

async function apply(promise, message) {
  try {
    ui.settings = await promise;
    ui.replacing = false;
    render();
    if (message) ui.notify(message);
    await ui.onChange();
    return true;
  } catch (error) {
    ui.notify(error.message, { error: true });
    return false;
  }
}

async function check(providerId) {
  ui.checks[providerId] = { state: "checking" };
  render();
  try {
    const { models } = await api.checkProvider(providerId);
    ui.checks[providerId] = { state: "ok", models };
  } catch (error) {
    ui.checks[providerId] = { state: "error", message: error.message };
  }
  if (ui.selected === providerId) render();
}

async function onSubmit(event) {
  const form = event.target.closest("[data-settings-form]");
  if (!form) return;
  event.preventDefault();
  const data = new FormData(form);
  const provider = form.dataset.provider;
  switch (form.dataset.settingsForm) {
    case "key": {
      const key = String(data.get("key") ?? "").trim();
      if (!key) return;
      form.reset();
      if (await apply(api.saveKey(provider, key), "Key saved.")) await check(provider);
      break;
    }
    case "use": {
      const model = String(data.get("model") ?? "").trim();
      try {
        await ui.saveDefaults({ provider_id: provider, model_id: model, system_prompt: ui.defaults?.system_prompt ?? "" });
        ui.defaults = { ...ui.defaults, provider_id: provider, model_id: model };
        await ui.onChange();
        render();
        ui.notify(`New runs use ${provider} / ${model}.`);
      } catch (error) {
        ui.notify(error.message, { error: true });
      }
      break;
    }
    case "endpoint": {
      const id = provider ?? String(data.get("id") ?? "").trim();
      const body = {
        base_url: String(data.get("base_url") ?? "").trim(),
        api_key_env: String(data.get("api_key_env") ?? "").trim() || null,
        models: String(data.get("models") ?? "").split(/[\n,]/).map((m) => m.trim()).filter(Boolean),
        json_mode: data.get("json_mode") === "on",
      };
      if (await apply(api.saveProvider(id, body), provider ? "Endpoint saved." : `Added ${id}.`)) {
        if (!provider) select(id);
      }
      break;
    }
    case "tool-server": {
      const id = form.dataset.server ?? String(data.get("id") ?? "").trim();
      const body = {
        command: String(data.get("command") ?? "").trim(),
        args: String(data.get("args") ?? "").split("\n").map((arg) => arg.trim()).filter(Boolean),
        cwd: String(data.get("cwd") ?? "").trim() || null,
        enabled: data.get("enabled") === "on",
      };
      await saveToolServer(id, body, form.dataset.server ? "Saved and restarted." : `Added ${id}.`);
      break;
    }
    case "review": {
      try {
        ui.lint = await api.saveLint(readReviewForm(form));
        ui.lintDraft = structuredClone(ui.lint.config.rules);
        render();
        ui.notify("Review settings saved; the next runs use them.");
        await ui.onChange();
      } catch (error) {
        ui.notify(error.message, { error: true });
      }
      break;
    }
    case "guard": {
      try {
        ui.tools = await api.saveGuard({ enabled: data.get("enabled") === "on", threshold: Number(data.get("threshold")) });
        render();
        ui.notify("Tool guard saved.");
      } catch (error) {
        ui.notify(error.message, { error: true });
      }
      break;
    }
    case "jev": {
      if (data.get("mode") === "offline") {
        await apply(api.clearJev(), "Jev classifies with the offline policy.");
      } else {
        const model = String(data.get("model") ?? "").trim();
        if (!model) {
          ui.notify("Choose a model for Jev.", { error: true });
          return;
        }
        await apply(api.saveJev({ provider: String(data.get("provider")), model }), `Jev classifies with ${model}.`);
      }
      break;
    }
  }
}

async function onClick(event) {
  const target = event.target.closest("[data-settings-select], [data-settings-action]");
  if (!target) return;
  if (target.dataset.settingsSelect) {
    select(target.dataset.settingsSelect);
    return;
  }
  const provider = ui.selected;
  switch (target.dataset.settingsAction) {
    case "reveal": {
      const input = $("key-input");
      if (input) input.type = input.type === "password" ? "text" : "password";
      break;
    }
    case "replace-key":
      ui.replacing = true;
      render();
      $("key-input")?.focus();
      break;
    case "cancel-replace":
      ui.replacing = false;
      render();
      break;
    case "remove-key":
      if (armed(target, "Click again to remove")) {
        delete ui.checks[provider];
        await apply(api.removeKey(provider), "Saved key removed.");
      }
      break;
    case "remove-provider":
      if (armed(target, "Click again to remove")) {
        await apply(api.removeProvider(provider), `Removed ${provider}.`);
      }
      break;
    case "check":
      await check(provider);
      break;
    case "rule-add":
    case "rule-remove":
    case "rules-reset": {
      const action = target.dataset.settingsAction;
      if (action === "rules-reset") ui.lintDraft = structuredClone(ui.lint.defaults);
      else {
        ui.lintDraft = readReviewForm(target.closest("form")).rules;
        if (action === "rule-add") ui.lintDraft.push({ id: "", text: "", applies: null, except: null });
        else ui.lintDraft.splice(Number(target.dataset.index), 1);
      }
      render();
      if (action === "rule-add") [...document.querySelectorAll("[name=rule_text]")].pop()?.focus();
      break;
    }
    case "tool-policy": {
      const { server, tool, policy } = target.dataset;
      try {
        ui.tools = await api.setToolPolicy(server, tool ? { tool, policy } : { policy });
        render();
        await ui.onChange();
      } catch (error) {
        ui.notify(error.message, { error: true });
      }
      break;
    }
    case "remove-tool-server":
      if (armed(target, "Click again to remove")) {
        try {
          ui.tools = await api.removeToolServer(target.dataset.server);
          ui.selected = "tool:new";
          render();
          ui.notify(`Removed ${target.dataset.server}.`);
          await ui.onChange();
        } catch (error) {
          ui.notify(error.message, { error: true });
        }
      }
      break;
    case "import-tool-server": {
      const candidate = ui.imports?.servers[Number(target.dataset.index)];
      if (candidate) {
        const body = { command: candidate.command, args: candidate.args, cwd: candidate.cwd, enabled: true };
        await saveToolServer(candidate.id, body, `Imported ${candidate.name}.`);
      }
      break;
    }
  }
}

/** Two-step confirmation for destructive buttons: the first click arms, the second acts. */
function armed(button, prompt) {
  if (button.dataset.armed) return true;
  button.dataset.armed = "true";
  const original = button.textContent;
  button.textContent = prompt;
  setTimeout(() => {
    if (button.isConnected) {
      delete button.dataset.armed;
      button.textContent = original;
    }
  }, 3000);
  return false;
}

async function saveToolServer(id, body, message) {
  ui.notify(`Starting ${id}…`);
  try {
    ui.tools = await api.saveToolServer(id, body);
    ui.selected = `tool:${id}`;
    ui.imports = await api.toolImports().catch(() => ui.imports);
    render();
    const status = ui.tools.capabilities?.servers.find((server) => server.id === id);
    ui.notify(status?.error ? `${id} could not start: ${status.error}` : message, { error: Boolean(status?.error) });
    await ui.onChange();
  } catch (error) {
    ui.notify(error.message, { error: true });
  }
}

let bound = false;
function bind() {
  if (bound) return;
  bound = true;
  const dialog = $("settings");
  dialog.addEventListener("submit", onSubmit);
  dialog.addEventListener("click", onClick);
  dialog.addEventListener("input", (event) => {
    if (!event.target.matches("[data-settings-filter]")) return;
    ui.toolFilter = event.target.value;
    const needle = ui.toolFilter.trim().toLowerCase();
    for (const row of dialog.querySelectorAll("[data-tool-row]")) {
      row.hidden = Boolean(needle) && !row.dataset.toolRow.includes(needle);
    }
  });
  dialog.addEventListener("change", (event) => {
    if (event.target.name === "mode") syncJevForm();
    if (event.target.id === "jev-provider") {
      const models = jevModels(ui.settings.providers.find((p) => p.id === event.target.value), knownModels(event.target.value));
      $("jev-models").innerHTML = models.map((model) => `<option value="${esc(model)}"></option>`).join("");
    }
  });
  $("settings-close").addEventListener("click", () => dialog.close());
}

/**
 * Opens the settings dialog at `section` (a provider ID, `jev`, or `new`). `defaults` are the
 * current project's run defaults and `saveDefaults(body)` changes them; `onChange` runs after
 * every saved change; `notify(message, {error})` reports outcomes.
 */
export async function openSettings({ section = null, defaults, saveDefaults, onChange, notify }) {
  bind();
  Object.assign(ui, { defaults, onChange, notify });
  if (saveDefaults) ui.saveDefaults = saveDefaults;
  if (section) ui.selected = section;
  ui.replacing = false;
  const dialog = $("settings");
  if (!dialog.open) dialog.showModal();
  render();
  try {
    [ui.settings, ui.tools, ui.imports, ui.lint] = await Promise.all([
      api.settings(),
      api.tools(),
      api.toolImports().catch(() => null),
      api.lint().catch(() => null),
    ]);
    ui.lintDraft = null;
    render();
  } catch (error) {
    notify(error.message, { error: true });
  }
}
