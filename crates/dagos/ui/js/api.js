// The DAGOS HTTP API and its live event stream. The UI reads recorded state through these calls
// only; it keeps no model of its own beyond what the API returns.

export class ApiError extends Error {
  constructor(message, status) {
    super(message);
    this.status = status;
  }
}

async function request(method, path, body) {
  const init = { method, headers: {} };
  if (body !== undefined) {
    init.headers["content-type"] = "application/json";
    init.body = JSON.stringify(body);
  }
  let response;
  try {
    response = await fetch(path, init);
  } catch {
    throw new ApiError("The DAGOS server is unreachable.", 0);
  }
  const data = await response.json().catch(() => ({}));
  if (!response.ok) {
    throw new ApiError(data.error ?? `${response.status} ${response.statusText}`, response.status);
  }
  return data;
}

export const api = {
  overview: () => request("GET", "/api/overview"),
  run: (id) => request("GET", `/api/runs/${encodeURIComponent(id)}`),
  start: (body) => request("POST", "/api/runs", body),
  saveConfig: (body) => request("PUT", "/api/config", body),
  recover: () => request("POST", "/api/recover"),
  projects: () => request("GET", "/api/projects"),
  createProject: (name) => request("POST", "/api/projects", { name }),
  renameProject: (id, name) => request("PATCH", `/api/projects/${encodeURIComponent(id)}`, { name }),
  projectOverview: (id) => request("GET", `/api/projects/${encodeURIComponent(id)}/overview`),
  saveProjectConfig: (id, body) => request("PUT", `/api/projects/${encodeURIComponent(id)}/config`, body),
  conversation: (id) => request("GET", `/api/conversations/${encodeURIComponent(id)}`),
  updateConversation: (id, body) => request("PATCH", `/api/conversations/${encodeURIComponent(id)}`, body),
  answerTool: (run, call, body) =>
    request("POST", `/api/runs/${encodeURIComponent(run)}/tools/${encodeURIComponent(call)}`, body),
  tools: () => request("GET", "/api/tools"),
  toolImports: () => request("GET", "/api/tools/import"),
  saveToolServer: (id, body) => request("PUT", `/api/tools/servers/${encodeURIComponent(id)}`, body),
  removeToolServer: (id) => request("DELETE", `/api/tools/servers/${encodeURIComponent(id)}`),
  setToolPolicy: (id, body) => request("PUT", `/api/tools/servers/${encodeURIComponent(id)}/policy`, body),
  settings: () => request("GET", "/api/settings"),
  saveKey: (id, key) => request("PUT", `/api/settings/providers/${encodeURIComponent(id)}/key`, { key }),
  removeKey: (id) => request("DELETE", `/api/settings/providers/${encodeURIComponent(id)}/key`),
  saveProvider: (id, body) => request("PUT", `/api/settings/providers/${encodeURIComponent(id)}`, body),
  removeProvider: (id) => request("DELETE", `/api/settings/providers/${encodeURIComponent(id)}`),
  checkProvider: (id) => request("POST", `/api/settings/providers/${encodeURIComponent(id)}/check`),
  saveJev: (body) => request("PUT", "/api/settings/jev", body),
  clearJev: () => request("DELETE", "/api/settings/jev"),
};

/**
 * Subscribes to committed events. `onEvent` receives each event; `onResync` fires when the stream
 * (re)connects or falls behind, meaning state should be reloaded; `onState` reports `live` or
 * `offline`. The browser reconnects on its own after a server restart.
 */
export function openStream({ onEvent, onResync, onState }) {
  const source = new EventSource("/api/stream");
  source.addEventListener("open", () => {
    onState("live");
    onResync();
  });
  source.addEventListener("event", (message) => onEvent(JSON.parse(message.data)));
  source.addEventListener("resync", () => onResync());
  source.addEventListener("error", () => onState("offline"));
  return () => source.close();
}
