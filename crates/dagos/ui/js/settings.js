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
  if (!ui.selected || (ui.selected !== "jev" && ui.selected !== "new" && !settings.providers.some((p) => p.id === ui.selected))) {
    ui.selected = settings.providers.find((p) => p.origin !== "builtin" && !p.available)?.id ?? settings.providers[1]?.id ?? "fake";
  }
  $("settings-nav").innerHTML = navHtml(settings, ui.selected);
  const detail = $("settings-detail");
  if (ui.selected === "jev") detail.innerHTML = jevHtml(settings, { checks: ui.checks });
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

let bound = false;
function bind() {
  if (bound) return;
  bound = true;
  const dialog = $("settings");
  dialog.addEventListener("submit", onSubmit);
  dialog.addEventListener("click", onClick);
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
    ui.settings = await api.settings();
    render();
  } catch (error) {
    notify(error.message, { error: true });
  }
}
