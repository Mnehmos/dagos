// Chat rendering: conversations as threads of turns, projects, and a small, safe markdown subset.
// Pure functions (view models in, escaped HTML out), tested under Node like view.js. Every turn is
// still a full DAGOS run; the chat only presents runs in order.

import { esc } from "./view.js";

// ---------------------------------------------------------------------------------------------
// Markdown: fenced code, inline code, bold, headings, lists, paragraphs, and http(s) links.
// Everything is escaped first, so model output can never inject markup.

function inlineHtml(text) {
  return String(text)
    .split(/(`[^`\n]+`)/)
    .map((part) => {
      if (part.length > 2 && part.startsWith("`") && part.endsWith("`")) {
        return `<code>${esc(part.slice(1, -1))}</code>`;
      }
      return esc(part)
        .replace(/\*\*([^*\n]+)\*\*/g, "<strong>$1</strong>")
        .replace(
          /\[([^\]\n]+)\]\((https?:\/\/[^\s)]+)\)/g,
          (_, label, url) => `<a href="${url}" target="_blank" rel="noopener noreferrer">${label}</a>`,
        );
    })
    .join("");
}

/** Renders a reply's markdown subset as HTML. */
export function markdownHtml(text) {
  const lines = String(text ?? "").replace(/\r\n?/g, "\n").split("\n");
  const out = [];
  let paragraph = [];
  let list = null;
  const flushParagraph = () => {
    if (paragraph.length) out.push(`<p>${paragraph.map(inlineHtml).join("<br>")}</p>`);
    paragraph = [];
  };
  const flushList = () => {
    if (list) out.push(`<${list.tag}>${list.items.map((item) => `<li>${inlineHtml(item)}</li>`).join("")}</${list.tag}>`);
    list = null;
  };
  for (let index = 0; index < lines.length; index += 1) {
    const line = lines[index];
    const fence = line.match(/^\s*```\s*([\w+-]*)\s*$/);
    if (fence) {
      flushParagraph();
      flushList();
      const code = [];
      for (index += 1; index < lines.length && !/^\s*```\s*$/.test(lines[index]); index += 1) code.push(lines[index]);
      const language = fence[1] ? ` data-lang="${esc(fence[1])}"` : "";
      out.push(`<pre class="code"${language}><code>${esc(code.join("\n"))}</code></pre>`);
      continue;
    }
    const heading = line.match(/^(#{1,4})\s+(.*)$/);
    const bullet = line.match(/^\s*[-*]\s+(.*)$/);
    const numbered = line.match(/^\s*\d+[.)]\s+(.*)$/);
    if (heading) {
      flushParagraph();
      flushList();
      out.push(`<h4>${inlineHtml(heading[2])}</h4>`);
    } else if (bullet || numbered) {
      flushParagraph();
      const tag = bullet ? "ul" : "ol";
      if (list?.tag !== tag) {
        flushList();
        list = { tag, items: [] };
      }
      list.items.push((bullet ?? numbered)[1]);
    } else if (!line.trim()) {
      flushParagraph();
      flushList();
    } else {
      flushList();
      paragraph.push(line);
    }
  }
  flushParagraph();
  flushList();
  return out.join("");
}

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

/** One turn: the user's message, then the reply (streaming, rendered, or failed). */
export function turnHtml(turn, { live = null } = {}) {
  const run = turn.run;
  const running = run.status === "running";
  const prose = running && live && live.length > turn.prose.length ? live : turn.prose;
  let reply;
  if (running) {
    reply = prose
      ? `<div class="bubble assistant streaming"><div class="prose-live" data-live-run="${esc(run.id)}">${esc(prose)}</div></div>`
      : `<div class="bubble assistant pending"><span class="typing" data-live-run="${esc(run.id)}" aria-label="Thinking"><i></i><i></i><i></i></span></div>`;
  } else if (run.status === "failed") {
    reply = `<div class="bubble assistant failed">${prose ? `<div class="md">${markdownHtml(prose)}</div>` : ""}${failureNoteHtml(turn)}</div>`;
  } else {
    reply = `<div class="bubble assistant"><div class="md">${prose ? markdownHtml(prose) : '<p class="empty-note">(no reply)</p>'}</div></div>`;
  }
  return `<article class="chat-turn status-${esc(run.status)}" data-turn="${esc(run.id)}">
    <div class="bubble user"><div class="md">${markdownHtml(turn.message ?? "")}</div></div>
    ${reply}
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
