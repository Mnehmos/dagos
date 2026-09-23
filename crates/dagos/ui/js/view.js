// Pure rendering: view models in, escaped HTML strings out. No DOM access, so rendering is tested
// under Node. Interactive elements carry data-* attributes that app.js handles by delegation.

import * as model from "./model.js";

const ENTITIES = { "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" };

/** Escapes text for HTML content and attribute values. */
export function esc(value) {
  return String(value ?? "").replace(/[&<>"']/g, (char) => ENTITIES[char]);
}

/** Pretty-printed, syntax-highlighted JSON. */
export function jsonHtml(value) {
  const text = esc(JSON.stringify(value, null, 2) ?? "null");
  return text.replace(
    /(&quot;(?:\\.|[^&\\]|&(?!quot;))*&quot;)(\s*:)?|\b(true|false|null)\b|-?\d+(?:\.\d+)?(?:[eE][+-]?\d+)?/g,
    (match, string, colon, literal) => {
      if (string) return colon ? `<span class="j-key">${string}</span>${colon}` : `<span class="j-str">${string}</span>`;
      if (literal) return `<span class="j-lit">${literal}</span>`;
      return `<span class="j-num">${match}</span>`;
    },
  );
}

/** The last characters of an ID, enough to tell nodes apart at a glance. */
export function shortId(id) {
  const [prefix, suffix = ""] = String(id).split("_");
  return suffix.length > 8 ? `${prefix}_…${suffix.slice(-6)}` : String(id);
}

function time(timestamp) {
  const date = new Date(timestamp);
  return Number.isNaN(date.getTime())
    ? ""
    : date.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" });
}


// ---------------------------------------------------------------------------------------------
// Inspector

export const TABS = [
  { key: "conversation", label: "Conversation" },
  { key: "context", label: "Context" },
  { key: "jev", label: "Jev" },
  { key: "ir", label: "IR" },
  { key: "response", label: "Response" },
  { key: "events", label: "Events" },
];

const STAGE_ICON = { done: "✓", failed: "✕", running: "", pending: "", skipped: "–" };

export function pipelineHtml(detail) {
  const stages = model.pipeline(detail.run, detail.events);
  const summaries = model.stageSummaries(detail);
  return `<ol class="pipeline" aria-label="Pipeline stages">${stages
    .map(
      (stage) => `<li class="stage stage-${stage.status}">
        <button type="button" class="stage-button" data-tab="${stage.tab}"
          aria-label="${esc(`${stage.label}: ${stage.status}. ${summaries[stage.key] ?? ""}`)}">
          <span class="stage-icon" aria-hidden="true">${STAGE_ICON[stage.status]}</span>
          <span class="stage-label">${esc(stage.label)}</span>
          <span class="stage-summary">${esc(summaries[stage.key] ?? "")}</span>
        </button></li>`,
    )
    .join("")}</ol>`;
}

function runHeaderHtml(detail) {
  const run = detail.run;
  const status = run.status;
  return `<header class="run-header">
    <div class="run-title">
      <span class="badge badge-${status}">${esc(status)}</span>
      <code class="run-id" title="${esc(run.id)}">${esc(run.id)}</code>
    </div>
    <dl class="run-facts">
      <div><dt>Provider</dt><dd>${esc(run.provider_id)}</dd></div>
      <div><dt>Model</dt><dd><code>${esc(run.model_id)}</code></dd></div>
      <div><dt>Started</dt><dd>${esc(time(run.started_at))}</dd></div>
      <div><dt>Duration</dt><dd>${esc(model.duration(run.started_at, run.completed_at) || "…")}</dd></div>
    </dl>
  </header>`;
}

export function inspectorHtml({ detail, dag, tab, streamed, streaming }) {
  const tabs = TABS.map(
    ({ key, label }, index) => `<button type="button" role="tab" id="tab-${key}" class="tab"
      data-tab="${key}" aria-selected="${key === tab}" aria-controls="panel"
      tabindex="${key === tab ? 0 : -1}" title="${esc(label)} (${index + 1})">${esc(label)}</button>`,
  ).join("");
  return `${runHeaderHtml(detail)}
    ${pipelineHtml(detail)}
    <div class="tabs" role="tablist" aria-label="Run details">${tabs}</div>
    <div id="panel" class="panel" role="tabpanel" aria-labelledby="tab-${tab}" tabindex="0">
      ${panelHtml({ detail, dag, tab, streamed, streaming })}
    </div>`;
}

function panelHtml({ detail, dag, tab, streamed, streaming }) {
  switch (tab) {
    case "context":
      return contextPanelHtml(detail, dag);
    case "jev":
      return jevPanelHtml(detail);
    case "ir":
      return irPanelHtml(detail);
    case "response":
      return responsePanelHtml(detail);
    case "events":
      return eventsPanelHtml(detail);
    default:
      return conversationPanelHtml(detail, streamed, streaming);
  }
}

function presentationHtml(prose, streaming) {
  return `<article class="turn presentation${streaming ? " streaming" : ""}">
    <header><span class="speaker">Assistant</span>
      <span class="tag tag-presentation" title="Streamed prose is shown to people and never stored as DAG state">Presentation · not DAG state</span></header>
    <p id="live-prose" class="prose">${esc(prose)}</p>
  </article>`;
}

function failureHtml(detail) {
  const failure = detail.failure;
  if (!failure) return "";
  const stage = { jev: "Jev output validation", response: "response validation" }[failure.rejected_stage];
  return `<section class="failure" role="alert">
    <header><span class="badge badge-failed">failed</span> <code>${esc(failure.error_code)}</code></header>
    ${stage ? "" : `<p>${esc(failure.message)}</p>`}
    ${
      stage
        ? `<dl class="facts"><div><dt>Rejected by</dt><dd>${esc(stage)}</dd></div>
           <div><dt>Reason</dt><dd><code>${esc(failure.rejected_reason)}</code></dd></div></dl>`
        : ""
    }
    ${
      detail.output != null
        ? `<details><summary>Raw provider output</summary><pre class="raw">${esc(detail.output)}</pre></details>`
        : ""
    }
    <p class="hint">Nothing from ${stage ? "rejected output" : "this run"} entered the DAG; everything recorded before the failure is intact.</p>
  </section>`;
}

function conversationPanelHtml(detail, streamed, streaming) {
  const prose = streamed ?? detail.streamed ?? "";
  const started = detail.events.some((event) => event.type === "inference.started");
  const emitted = detail.emitted_nodes.length + detail.emitted_edges.length;
  return `<div class="conversation">
    <article class="turn user"><header><span class="speaker">You</span>
      ${detail.message ? `<code class="node-ref" title="Durable conversation node">${esc(shortId(detail.message.node_id))}</code>` : ""}</header>
      <p class="prose">${esc(detail.message?.text ?? "")}</p></article>
    ${started || prose ? presentationHtml(prose, streaming) : `<p class="waiting">${streaming ? "Waiting for the provider…" : "The run never reached the provider."}</p>`}
    ${failureHtml(detail)}
    ${
      detail.response
        ? `<section class="canonical-summary"><span class="tag tag-canonical">Canonical · validated · durable</span>
           <p>${emitted ? `${detail.emitted_nodes.length} node(s) and ${detail.emitted_edges.length} edge(s) became durable DAG state.` : "The response recorded no emissions."}
           <button type="button" class="link" data-tab="response">Inspect the response</button></p></section>`
        : ""
    }
  </div>`;
}

function nodeCellHtml(node, nodeId) {
  if (!node) return `<code>${esc(shortId(nodeId))}</code>`;
  return `<button type="button" class="node-link" data-node="${esc(node.id)}">
    <span class="type-dot type-${esc(node.type)}" aria-hidden="true"></span>
    <span class="node-text">${esc(model.excerpt(model.nodeLabel(node), 70))}</span></button>`;
}

function contextPanelHtml(detail, dag) {
  const view = model.contextView(detail, dag);
  const rows = view.members
    .map(
      (member) => `<tr${member.added ? ' class="added"' : ""}>
        <td class="num">${member.ordering + 1}</td>
        <td class="grow">${nodeCellHtml(member.node, member.node_id)}</td>
        <td><span class="type-name">${esc(member.node?.type ?? "")}</span></td>
        <td><span class="source source-${esc(member.source)}" title="${member.source === "jev" ? "Classified active by Jev in this run" : "Carried from the previous run's context"}">${esc(member.source)}</span>${member.added ? ' <span class="badge badge-added">added</span>' : ""}</td>
      </tr>`,
    )
    .join("");
  const removed = view.removed
    .map(
      (entry) => `<li>${nodeCellHtml(entry.node, entry.nodeId)} <span class="badge badge-durable">still durable</span></li>`,
    )
    .join("");
  return `<p class="callout callout-temporary"><strong>Temporary projection.</strong> This run's active context is what compiled into its IR. Removing a node from context never deletes it from the durable DAG.</p>
    ${
      view.members.length
        ? `<table class="grid"><thead><tr><th>#</th><th class="grow">Node</th><th>Type</th><th>Why active</th></tr></thead><tbody>${rows}</tbody></table>`
        : `<p class="empty-note">The active context is empty${detail.classification ? "" : " (Jev has not classified yet)"}.</p>`
    }
    ${removed ? `<h3>Removed by Jev in this run</h3><ul class="removed">${removed}</ul>` : ""}
    <p class="hint">${view.carried.length} node(s) were carried from the previous run before Jev classified.</p>`;
}

function jevPanelHtml(detail) {
  if (!detail.jev_request) {
    return `<p class="empty-note">Jev has not been asked yet.</p>`;
  }
  const view = model.jevView(detail);
  const rows = view.rows
    .map(
      (row) => `<tr class="effect-${esc(row.effect.split(" ")[0])}">
        <td class="grow">${nodeCellHtml({ id: row.candidate.node_id, type: row.candidate.type, payload: row.candidate.payload }, row.candidate.node_id)}</td>
        <td><span class="type-name">${esc(row.candidate.type)}</span></td>
        <td>${row.candidate.in_context ? "in context" : "—"}</td>
        <td>${row.label ? `<span class="label label-${esc(row.label)}">${esc(row.label)}</span>` : "—"}</td>
        <td>${esc(row.effect)}</td>
      </tr>`,
    )
    .join("");
  const rejected = detail.failure?.rejected_stage === "jev";
  const fallback = detail.jev_fallback;
  return `<p class="callout"><strong>Jev decides the active context.</strong> It labels candidate nodes <em>active</em> or <em>inactive</em>, and the runtime applies those labels as this run's context. It cannot write to the DAG, answer, or choose providers.</p>
    <dl class="facts compact"><div><dt>Classified by</dt><dd><code>${esc(fallback?.jev_id ?? detail.jev_id ?? "—")}</code>${fallback ? " (fallback)" : ""}</dd></div></dl>
    ${
      fallback
        ? `<div class="callout callout-warn"><p><strong><code>${esc(fallback.from ?? "The model Jev")}</code> could not be used</strong>, so the offline policy classified instead: ${esc(fallback.reason)}</p>${
            fallback.rejected_output != null
              ? `<details><summary>Rejected output (never applied)</summary><pre class="json">${esc(fallback.rejected_output)}</pre></details>`
              : ""
          }</div>`
        : ""
    }
    ${rejected ? `<p class="callout callout-error">Jev's output was rejected: <code>${esc(detail.failure.rejected_reason)}</code>. Nothing was applied.</p>` : ""}
    ${
      view.candidates
        ? `<table class="grid"><thead><tr><th class="grow">Candidate</th><th>Type</th><th>Before</th><th>Label</th><th>Effect</th></tr></thead><tbody>${rows}</tbody></table>`
        : `<p class="empty-note">There were no candidates: the run's own message is its task, and the project had no other nodes.</p>`
    }
    <details><summary>Request <code>kiss.jev-request.v1</code></summary><pre class="json">${jsonHtml(detail.jev_request)}</pre></details>
    ${detail.classification ? `<details><summary>Output <code>kiss.jev-context.v1</code></summary><pre class="json">${jsonHtml(detail.classification)}</pre></details>` : ""}`;
}

function irPanelHtml(detail) {
  const ir = detail.ir;
  if (!ir) {
    return `<p class="empty-note">No IR: the run stopped before compilation${detail.failure ? ` (<code>${esc(detail.failure.error_code)}</code>)` : ""}.</p>`;
  }
  const chips = [
    `${ir.system_prompt.length} chars of system prompt`,
    `${ir.context.length} context node(s)`,
    `${ir.recent_events.length} recent run outcome(s)`,
    `${(ir.tools ?? []).length} tool description(s)`,
  ];
  return `<p class="callout"><strong>Exactly what the provider received</strong>, as <code>${esc(ir.schema)}</code>. Storage records never cross this boundary.</p>
    <ul class="chips">${chips.map((chip) => `<li>${esc(chip)}</li>`).join("")}</ul>
    <div class="code-toolbar"><button type="button" data-copy="ir">Copy IR JSON</button></div>
    <pre class="json" id="ir-json">${jsonHtml(ir)}</pre>`;
}

function emissionHtml({ emission, durableId }) {
  if (emission.kind === "node") {
    return `<li class="emission"><span class="type-dot type-${esc(emission.type)}" aria-hidden="true"></span>
      <code>${esc(emission.ref)}</code> <span class="type-name">${esc(emission.type)}</span>
      → ${durableId ? `<button type="button" class="node-link" data-node="${esc(durableId)}"><code>${esc(shortId(durableId))}</code></button>` : "<em>not created</em>"}
      <span class="node-text">${esc(model.excerpt(model.nodeLabel({ type: emission.type, payload: emission.payload }), 60))}</span></li>`;
  }
  return `<li class="emission edge"><code>${esc(shortId(emission.from))}</code>
    <span class="edge-type edge-${esc(emission.type)}">${esc(emission.type)}</span>
    <code>${esc(shortId(emission.to))}</code>${durableId ? ` <span class="durable-id">${esc(shortId(durableId))}</span>` : ""}</li>`;
}

function responsePanelHtml(detail) {
  const response = detail.response;
  if (!response) {
    return detail.output != null
      ? failureHtml(detail) || `<pre class="raw">${esc(detail.output)}</pre>`
      : `<p class="empty-note">No response${detail.run.status === "running" ? " yet" : ""}.</p>`;
  }
  const emissions = model.emissionView(detail);
  const toolCalls = response.tool_calls ?? [];
  return `<div class="split">
      <section class="presentation-card"><span class="tag tag-presentation">Presentation · not DAG state</span>
        <p class="prose">${esc(response.presentation.prose)}</p></section>
      <section class="canonical-card"><span class="tag tag-canonical">Canonical · validated · durable</span>
        ${emissions.length ? `<ul class="emissions">${emissions.map(emissionHtml).join("")}</ul>` : `<p class="empty-note">No emissions.</p>`}
      </section>
    </div>
    ${
      toolCalls.length
        ? `<section class="tool-calls"><h3>Requested tool calls</h3><p class="hint">Recorded for inspection; DAGOS v0.1 never executes them.</p><pre class="json">${jsonHtml(toolCalls)}</pre></section>`
        : ""
    }
    <details><summary>Validated response <code>${esc(response.schema)}</code></summary><pre class="json">${jsonHtml(response)}</pre></details>
    <details><summary>Raw provider output</summary><pre class="raw">${esc(detail.output ?? "")}</pre></details>`;
}

function eventsPanelHtml(detail) {
  const groups = model.eventGroups(detail.events);
  const items = groups
    .map((group) => {
      const sequences = group.count > 1 ? `#${group.sequence}–${group.lastSequence}` : `#${group.sequence}`;
      const body =
        group.type === "inference.delta"
          ? `<span class="delta-text">${esc(model.excerpt(group.text, 120))}</span>`
          : `<details><summary>payload</summary><pre class="json">${jsonHtml(group.event.payload)}</pre></details>`;
      return `<li class="event cat-${esc(model.eventCategory(group.type))}">
        <span class="seq">${sequences}</span>
        <span class="etype">${esc(group.type)}${group.count > 1 ? ` <span class="count">×${group.count}</span>` : ""}</span>
        <span class="offset">+${group.offset} ms</span>
        <div class="event-body">${body}</div></li>`;
    })
    .join("");
  return `<p class="callout">The run's ordered, append-only history: every transition above is one of these events.</p>
    <ol class="events">${items}</ol>`;
}

export function emptyInspectorHtml(overview) {
  const nodes = overview?.dag?.nodes?.length ?? 0;
  const connected = (overview?.providers ?? []).some((provider) => provider.id !== "fake");
  return `<div class="welcome">
    <h2>Nothing to inspect yet</h2>
    <p>Send a message to start a run. With the <code>fake</code> provider everything works offline.</p>
    ${
      connected
        ? ""
        : `<div class="connect-card"><p><strong>Connect a model.</strong> Paste an OpenRouter, OpenAI, or Z.ai key — or add a local endpoint — and pick a model. Jev can use one too.</p><button type="button" class="primary" data-action="settings">Add an API key</button></div>`
    }
    <ol class="flow" aria-label="The DAGOS pipeline">
      <li>message</li><li>Jev classifies context</li><li>active context</li><li>versioned IR</li>
      <li>provider</li><li>validated response</li><li>DAG emissions</li><li>events</li>
    </ol>
    <p class="hint">${nodes ? `The durable DAG already holds ${nodes} node(s).` : "The durable DAG is empty."} Try model <code>fake-malformed</code> or <code>fake-cycle</code> to see failures stay explicit.</p>
  </div>`;
}

// ---------------------------------------------------------------------------------------------
// Durable DAG

const ROW = 34;
const GUTTER = 72;

function arcPath(arc, maxSpan) {
  const y1 = arc.from * ROW + ROW / 2;
  const y2 = arc.to * ROW + ROW / 2;
  const bulge = 10 + ((GUTTER - 16) * arc.span) / Math.max(1, maxSpan);
  const x = GUTTER - 2;
  return `M ${x} ${y1} C ${x - bulge} ${y1}, ${x - bulge} ${y2}, ${x} ${y2}`;
}

export function dagHtml({ dag, active, emitted, selectedNodeId, runId, memberSources }) {
  const layout = model.dagLayout(dag, { active, emitted });
  const header = `<header class="panel-header"><h2>Durable DAG</h2>
    <span class="counts">${dag.nodes.length} nodes · ${dag.edges.length} edges</span></header>
    <ul class="legend" aria-label="Legend">
      <li><span class="swatch swatch-durable" aria-hidden="true"></span>durable node</li>
      <li><span class="swatch swatch-active" aria-hidden="true"></span>active in ${runId ? "selected run's" : "a run's"} context · temporary</li>
      <li><span class="swatch swatch-emitted" aria-hidden="true"></span>emitted by selected run</li>
    </ul>`;
  if (!dag.nodes.length) {
    return `${header}<p class="empty-note">The durable DAG is empty. Each run records its message here, and valid emissions add nodes and edges.</p>`;
  }
  const height = layout.rows.length * ROW;
  const arcs = layout.arcs
    .map((arc) => {
      const highlighted =
        selectedNodeId && (arc.edge.from_node_id === selectedNodeId || arc.edge.to_node_id === selectedNodeId);
      return `<path class="arc edge-${esc(arc.edge.type)}${highlighted ? " hl" : ""}" d="${arcPath(arc, layout.maxSpan)}" marker-end="url(#arrow)"><title>${esc(arc.edge.type)}</title></path>`;
    })
    .join("");
  const rows = layout.rows
    .map((row) => {
      const classes = ["dag-row", `type-${row.node.type}`];
      if (row.active) classes.push("active");
      if (row.emitted) classes.push("emitted");
      if (row.node.id === selectedNodeId) classes.push("selected");
      const flags = [row.active ? "active in context" : "", row.emitted ? "emitted by this run" : ""].filter(Boolean);
      return `<li><button type="button" class="${classes.join(" ")}" data-node="${esc(row.node.id)}"
        aria-label="${esc(`${row.node.type}: ${row.label}${flags.length ? ` (${flags.join(", ")})` : ""}`)}"${row.node.id === selectedNodeId ? ' aria-pressed="true"' : ""}>
        <span class="type-dot type-${esc(row.node.type)}" aria-hidden="true"></span>
        <span class="node-text">${esc(row.label)}</span>
        ${row.emitted ? '<span class="mini-badge">new</span>' : ""}
        <span class="node-id">${esc(shortId(row.node.id))}</span></button></li>`;
    })
    .join("");
  const detail = selectedNodeId ? nodeDetailHtml(dag, selectedNodeId, active, memberSources) : "";
  return `${header}
    <div class="dag-graph">
      <svg class="dag-arcs" width="${GUTTER}" height="${height}" viewBox="0 0 ${GUTTER} ${height}" aria-hidden="true" focusable="false">
        <defs><marker id="arrow" viewBox="0 0 8 8" refX="7" refY="4" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L8,4 L0,8 z"></path></marker></defs>
        ${arcs}
      </svg>
      <ol class="dag-rows">${rows}</ol>
    </div>
    ${detail}`;
}

function nodeDetailHtml(dag, nodeId, active, memberSources) {
  const node = dag.nodes.find((candidate) => candidate.id === nodeId);
  if (!node) return "";
  const { incoming, outgoing } = model.neighbours(dag, nodeId);
  const relation = (edge, other, direction) =>
    `<li>${direction === "out" ? "this" : nodeCellHtml(other, other?.id)} <span class="edge-type edge-${esc(edge.type)}">${esc(edge.type)}</span> ${direction === "out" ? nodeCellHtml(other, other?.id) : "this"}</li>`;
  const source = memberSources?.get(nodeId);
  return `<section class="node-detail" aria-label="Selected node">
    <header><span class="type-dot type-${esc(node.type)}" aria-hidden="true"></span>
      <strong>${esc(node.type)}</strong> <code>${esc(node.id)}</code>
      <button type="button" class="icon-button" data-close-node aria-label="Close node details">×</button></header>
    <p class="membership ${active.has(nodeId) ? "is-active" : ""}">${
      active.has(nodeId)
        ? `Active in the selected run's context${source ? ` (source: ${esc(source)})` : ""}.`
        : "Not in the selected run's active context. Still durable."
    }</p>
    <pre class="json">${jsonHtml(node.payload)}</pre>
    ${outgoing.length || incoming.length ? `<ul class="relations">${outgoing.map(({ edge, node: other }) => relation(edge, other, "out")).join("")}${incoming.map(({ edge, node: other }) => relation(edge, other, "in")).join("")}</ul>` : `<p class="hint">No relations.</p>`}
    <p class="hint">Created ${esc(time(node.created_at))}</p>
  </section>`;
}

// ---------------------------------------------------------------------------------------------
// Status footer

export function statusHtml(overview, connection) {
  const connectionLabel = { live: "Live", connecting: "Connecting…", offline: "Reconnecting…" }[connection];
  const capabilities = overview?.capabilities;
  let mcp = "not configured";
  if (capabilities) {
    const failed = capabilities.servers.filter((server) => server.error).length;
    mcp = `${capabilities.tools.length} tool(s)${failed ? `, ${failed} server(s) unavailable` : ""}`;
  }
  return `<dl class="facts compact">
    <div><dt>Jev</dt><dd><button type="button" class="link" data-action="settings" data-section="jev" title="Choose how Jev classifies">${esc(overview?.jev ?? "…")}</button></dd></div>
    <div><dt>Providers</dt><dd>${esc((overview?.providers ?? []).map((provider) => provider.id).join(", ") || "…")}</dd></div>
    <div><dt>MCP</dt><dd>${esc(mcp)}</dd></div>
    <div><dt>Stream</dt><dd class="connection-${connection}">${esc(connectionLabel)}</dd></div>
  </dl>`;
}
