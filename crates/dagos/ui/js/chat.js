// Chat rendering: conversations as threads of turns, projects, and a small, safe markdown subset.
// Pure functions (view models in, escaped HTML out), tested under Node like view.js. Every turn is
// still a full DAGOS run; the chat only presents runs in order.

import { markdownHtml } from "./markdown.js";
import { esc, jsonHtml } from "./view.js";

export { markdownHtml };

// ---------------------------------------------------------------------------------------------
// Time

/** A short relative time such as `just now`, `5m`, `3h`, `2d`, or a date. */
export function relativeTime(timestamp, now = Date.now()) {
  const then = new Date(timestamp).getTime();
  if (Number.isNaN(then)) return "";
  const seconds = Math.max(0, Math.round((now - then) / 1000));
  if (seconds < 45) return "just now";
  if (seconds < 3600) return `${Math.round(seconds / 60)}m`;
  if (seconds < 86400) return `${Math.round(seconds / 3600)}h`;
  if (seconds < 7 * 86400) return `${Math.round(seconds / 86400)}d`;
  return new Date(then).toLocaleDateString([], { month: "short", day: "numeric" });
}

// ---------------------------------------------------------------------------------------------
// Sidebar

/** The conversation list: active conversations, then (if any) archived ones in a disclosure. */
export function conversationListHtml(conversations, selectedId, { now = Date.now(), showArchived = false } = {}) {
  const item = (conversation) => {
    const selected = conversation.id === selectedId;
    return `<li><button type="button" class="conv-item${selected ? " selected" : ""}" data-conversation="${esc(conversation.id)}"${selected ? ' aria-current="true"' : ""}>
      <span class="conv-title">${esc(conversation.title)}</span>
      <span class="conv-time">${esc(relativeTime(conversation.updated_at, now))}</span>
    </button></li>`;
  };
  const active = conversations.filter((conversation) => !conversation.archived_at);
  const archived = conversations.filter((conversation) => conversation.archived_at);
  const list = active.length
    ? `<ol class="conv-list">${active.map(item).join("")}</ol>`
    : `<p class="empty-note">No chats yet. Your first message starts one.</p>`;
  const archive = archived.length
    ? `<details class="archived"${showArchived ? " open" : ""}><summary>Archived (${archived.length})</summary><ol class="conv-list">${archived.map(item).join("")}</ol></details>`
    : "";
  return list + archive;
}

/** The project menu: every project, then forms to create one or rename the current one. */
export function projectMenuHtml(projects, currentId) {
  const items = projects
    .map(({ project, conversations, nodes }) => {
      const current = project.id === currentId;
      return `<li><button type="button" class="menu-item${current ? " selected" : ""}" data-project="${esc(project.id)}" role="menuitemradio" aria-checked="${current}">
        <span class="menu-title">${esc(project.name)}</span>
        <span class="menu-meta">${conversations} chat${conversations === 1 ? "" : "s"} · ${nodes} node${nodes === 1 ? "" : "s"}</span>
      </button></li>`;
    })
    .join("");
  const current = projects.find(({ project }) => project.id === currentId)?.project;
  return `<p class="section-label">Projects</p>
    <ul class="menu-list" role="menu">${items}</ul>
    <p class="hint">Each project has its own durable DAG; its chats share it.</p>
    <form class="menu-form" data-form="new-project"><input name="name" placeholder="New project name" autocomplete="off" required aria-label="New project name"><button type="submit" class="primary">Create</button></form>
    ${current ? `<form class="menu-form" data-form="rename-project"><input name="name" value="${esc(current.name)}" autocomplete="off" required aria-label="Rename this project"><button type="submit">Rename</button></form>` : ""}`;
}

// ---------------------------------------------------------------------------------------------
// Thread

/** The chat header: title (click to rename), then actions. */
export function chatHeaderHtml(conversation, { renaming = false } = {}) {
  if (!conversation) {
    return `<h2 class="chat-title">New chat</h2>`;
  }
  const title = renaming
    ? `<form class="rename-form" data-form="rename-conversation"><input name="title" value="${esc(conversation.title)}" autocomplete="off" required aria-label="Chat title"><button type="submit" class="primary">Save</button><button type="button" data-action="cancel-rename">Cancel</button></form>`
    : `<h2 class="chat-title"><button type="button" class="title-button" data-action="rename" title="Rename">${esc(conversation.title)}</button></h2>`;
  const archive = conversation.archived_at
    ? `<button type="button" data-action="restore">Restore</button>`
    : `<button type="button" data-action="archive" title="Hide this chat; its runs stay durable">Archive</button>`;
  return `${title}<div class="chat-actions">${archive}</div>`;
}

function turnMetaHtml(turn) {
  const run = turn.run;
  const parts = [`<span title="Provider / model">${esc(run.provider_id)} / ${esc(run.model_id)}</span>`];
  parts.push(`<span title="Durable nodes in the active context the model saw">${turn.context_size} in context</span>`);
  if (turn.jev_id) {
    parts.push(
      turn.jev_fallback
        ? `<span class="meta-warn" title="The model Jev could not classify; the offline policy did">Jev fallback</span>`
        : `<span title="Who classified the context">${esc(turn.jev_id.replace(/-jev:/, " · "))}</span>`,
    );
  }
  if (turn.emitted_nodes || turn.emitted_edges) {
    parts.push(`<span class="meta-canonical" title="Validated emissions that became durable DAG state">+${turn.emitted_nodes} node${turn.emitted_nodes === 1 ? "" : "s"}${turn.emitted_edges ? ` · +${turn.emitted_edges} edge${turn.emitted_edges === 1 ? "" : "s"}` : ""}</span>`);
  }
  if (turn.tools_offered) {
    parts.push(`<span title="Tools Jev exposed to the model this turn, of those offered">tools ${turn.tools_exposed}/${turn.tools_offered}</span>`);
  }
  if (turn.recalled) {
    parts.push(`<span title="Earlier turns (from any chat of the project) Jev judged relevant and brought back into the model's context">recalled ${turn.recalled}</span>`);
  }
  if (turn.omitted_results) {
    parts.push(`<span title="Large earlier tool results Jev left out of the model's context because the current step did not need them">${turn.omitted_results} result${turn.omitted_results === 1 ? "" : "s"} left out</span>`);
  }
  const tools = (turn.items ?? []).filter((item) => item.kind === "tool").length;
  if (tools) parts.push(`<span title="Tool calls in this turn">${tools} tool call${tools === 1 ? "" : "s"}</span>`);
  parts.push(`<button type="button" class="link" data-inspect="${esc(run.id)}">Inspect</button>`);
  return `<div class="turn-meta">${parts.join('<span class="dot" aria-hidden="true">·</span>')}</div>`;
}

function failureNoteHtml(turn) {
  const failure = turn.failure;
  if (!failure) return "";
  const why = failure.rejected_reason ?? failure.message;
  const stage = { jev: "Jev's output was rejected", response: "The reply was rejected" }[failure.rejected_stage];
  return `<div class="chat-failure" role="alert"><strong>${esc(stage ?? "The run failed")}</strong> <code>${esc(failure.error_code)}</code><p>${esc(why)}</p><p class="hint">Nothing from it entered the DAG.</p></div>`;
}

const TOOL_STATUS = {
  awaiting: "waiting",
  running: "running…",
  completed: "done",
  failed: "failed",
  denied: "denied",
};

/** A tool call's output as readable text: its text parts, an error, or JSON. */
export function toolOutputText(output) {
  if (output == null) return "";
  if (typeof output.error === "string") return output.error;
  const parts = Array.isArray(output.content) ? output.content : [];
  const texts = parts.map((part) => (part.type === "text" ? part.text : part.note ?? `[${part.type}${part.uri ? ` ${part.uri}` : ""}]`));
  if (texts.length) return texts.join("\n\n");
  return JSON.stringify(output.structured ?? output, null, 2);
}

/** A tool call's output as HTML: text parts as markdown (without a leading heading that only
 * repeats the tool's name), structured-only output as JSON. */
export function toolOutputHtml(call) {
  const output = call.output;
  if (output == null) return "";
  const hasText = typeof output.error === "string" || (Array.isArray(output.content) && output.content.length);
  if (!hasText) return `<pre class="json">${jsonHtml(output.structured ?? output)}</pre>`;
  const tool = call.name.split(".").pop();
  const lines = toolOutputText(output).split("\n");
  if (/^#{1,6}\s/.test(lines[0] ?? "") && lines[0].includes(tool)) lines.shift();
  return `<div class="md tool-md">${markdownHtml(lines.join("\n"))}</div>`;
}

/** One tool call in a turn; calls waiting for a person get Allow / Deny buttons. */
export function toolCardHtml(call, runId) {
  const pending = call.pending && call.status === "awaiting";
  const status = pending ? "needs approval" : TOOL_STATUS[call.status] ?? call.status;
  const hasArguments = call.arguments && Object.keys(call.arguments).length;
  const output = toolOutputHtml(call);
  const approval = pending
    ? `<div class="approval" role="group" aria-label="Approve ${esc(call.name)}">
        <span>Run <code>${esc(call.name)}</code>?</span>
        <button type="button" class="primary" data-approve="allow" data-run="${esc(runId)}" data-call="${esc(call.call_id)}">Allow once</button>
        <button type="button" data-approve="always" data-run="${esc(runId)}" data-call="${esc(call.call_id)}" title="Set this tool to allow; later calls run without asking">Always allow</button>
        <button type="button" class="danger" data-approve="deny" data-run="${esc(runId)}" data-call="${esc(call.call_id)}">Deny</button>
      </div>`
    : "";
  return `<div class="tool-call status-${esc(call.status)}${pending ? " pending" : ""}" data-card="${esc(`${runId}:${call.call_id}`)}">
    <details${pending ? " open" : ""}>
      <summary><span class="tool-icon" aria-hidden="true">⚙</span><code class="tool-name">${esc(call.name)}</code><span class="tool-status">${esc(status)}</span></summary>
      <div class="tool-body">
        ${hasArguments ? `<p class="section-label">Arguments</p><pre class="json">${jsonHtml(call.arguments)}</pre>` : `<p class="hint">No arguments.</p>`}
        ${call.reason ? `<p class="tool-reason">${esc(call.reason)}</p>` : ""}
        ${output ? `<p class="section-label">${call.status === "failed" ? "Error" : "Result"}</p>${output}` : ""}
      </div>
    </details>
    ${approval}
  </div>`;
}

/** Consecutive tool calls: shown as they are when there are few, otherwise folded into one group
 * that opens by itself while a call waits for a person. */
export function toolGroupHtml(calls, runId) {
  const cards = calls.map((call) => toolCardHtml(call, runId)).join("");
  if (calls.length < 3) return cards;
  const counts = {};
  for (const call of calls) counts[call.status] = (counts[call.status] ?? 0) + 1;
  const summary = ["completed", "failed", "denied", "running", "awaiting"]
    .filter((status) => counts[status])
    .map((status) => `<span class="group-${status}">${counts[status]} ${TOOL_STATUS[status]}</span>`)
    .join(" · ");
  const open = calls.some((call) => call.pending || call.status === "running");
  const names = [...new Set(calls.map((call) => call.name.split(".").pop()))];
  const shown = names.slice(0, 4).map((name) => `<code>${esc(name)}</code>`).join(", ");
  return `<details class="tool-group" data-card="${esc(`${runId}:group:${calls[0].call_id}`)}"${open ? " open" : ""}>
    <summary><span class="tool-icon" aria-hidden="true">⚙</span><strong>${calls.length} tool calls</strong><span class="group-names">${shown}${names.length > 4 ? ` +${names.length - 4} more` : ""}</span><span class="group-summary">${summary}</span></summary>
    <div class="tool-group-body">${cards}</div>
  </details>`;
}

/** One turn: the user's message, then each reply and tool call in order (streaming, rendered, or failed). */
export function turnHtml(turn, { live = null } = {}) {
  const run = turn.run;
  const running = run.status === "running";
  const items = turn.items ?? (turn.prose ? [{ kind: "prose", text: turn.prose }] : []);
  const steps = [];
  for (let index = 0; index < items.length; ) {
    if (items[index].kind !== "tool") {
      steps.push(`<div class="bubble assistant"><div class="md">${markdownHtml(items[index].text)}</div></div>`);
      index += 1;
      continue;
    }
    const calls = [];
    while (index < items.length && items[index].kind === "tool") calls.push(items[index++]);
    steps.push(toolGroupHtml(calls, run.id));
  }
  if (running) {
    const streamed = live && live.length > (turn.streaming ?? "").length ? live : turn.streaming ?? "";
    const waiting = items.some((item) => item.kind === "tool" && item.pending);
    if (streamed && !waiting) {
      steps.push(`<div class="bubble assistant streaming"><div class="prose-live" data-live-run="${esc(run.id)}">${esc(streamed)}</div></div>`);
    } else if (!waiting) {
      steps.push(`<div class="bubble assistant pending"><span class="typing" data-live-run="${esc(run.id)}" aria-label="Thinking"><i></i><i></i><i></i></span></div>`);
    }
  } else if (run.status === "failed") {
    steps.push(`<div class="bubble assistant failed">${failureNoteHtml(turn)}</div>`);
  } else if (!items.length) {
    steps.push(`<div class="bubble assistant"><div class="md"><p class="empty-note">(no reply)</p></div></div>`);
  }
  return `<article class="chat-turn status-${esc(run.status)}" data-turn="${esc(run.id)}">
    <div class="bubble user"><div class="md">${markdownHtml(turn.message ?? "")}</div></div>
    ${steps.join("")}
    ${running ? "" : turnMetaHtml(turn)}
  </article>`;
}

/** Every turn of a conversation, oldest first. `live` maps a running run's ID to its streamed prose. */
export function threadHtml(view, { live = {} } = {}) {
  if (!view?.turns?.length) return "";
  return view.turns.map((turn) => turnHtml(turn, { live: live[turn.run.id] })).join("");
}

/** The empty state of a new chat. */
export function newChatHtml(overview) {
  const project = overview?.project?.name ?? "this project";
  const nodes = overview?.dag?.nodes?.length ?? 0;
  const connected = (overview?.providers ?? []).some((provider) => provider.id !== "fake");
  return `<div class="chat-empty">
    <h2>What are we working on in ${esc(project)}?</h2>
    <p>Every message is a DAGOS run: Jev picks the relevant durable nodes, the model receives versioned IR with this chat's recent turns, and only validated emissions become durable.</p>
    <p class="hint">${nodes ? `This project's DAG holds ${nodes} node${nodes === 1 ? "" : "s"} that any chat can draw on.` : "This project's DAG is empty; it grows as replies record decisions, tasks, and results."}</p>
    ${
      connected
        ? ""
        : `<div class="connect-card"><p><strong>Connect a model.</strong> Paste an OpenRouter, OpenAI, or Z.ai key — or add a local endpoint. Until then, the offline <code>fake</code> provider answers.</p><button type="button" class="primary" data-action="settings">Add an API key</button></div>`
    }
  </div>`;
}
