// Pure view models derived from the DAGOS API. No DOM access here, so every function can be tested
// under Node against JSON produced by the real runtime.

/** The pipeline, in order. `tab` is where a stage's details live in the inspector. */
export const STAGES = [
  { key: "message", label: "Message", tab: "conversation" },
  { key: "carry", label: "Carry", tab: "context" },
  { key: "jev", label: "Jev", tab: "jev" },
  { key: "context", label: "Context", tab: "context" },
  { key: "ir", label: "IR", tab: "ir" },
  { key: "provider", label: "Provider", tab: "conversation" },
  { key: "validation", label: "Validation", tab: "response" },
  { key: "emissions", label: "Emissions", tab: "response" },
  { key: "outcome", label: "Outcome", tab: "events" },
];

/** The event that marks each stage as done. */
const COMPLETED_BY = {
  message: "message.recorded",
  carry: "context.carried",
  jev: "jev.classified",
  context: "jev.classified",
  ir: "ir.compiled",
  provider: "inference.completed",
  validation: "response.validated",
  emissions: "response.validated",
};

/** Where each error code stops the pipeline. Other codes stop at the first unfinished stage. */
const FAILS_AT = {
  jev_failed: "jev",
  jev_invalid_output: "jev",
  ir_invalid: "ir",
  provider_failed: "provider",
  provider_timeout: "provider",
  response_invalid: "validation",
  emission_rejected: "emissions",
};

/**
 * Each stage's status for a run: `done`, `running`, `pending`, `failed`, or `skipped` (never reached
 * because an earlier stage failed).
 */
export function pipeline(run, events) {
  const seen = new Set(events.map((event) => event.type));
  const done = (key) => seen.has(COMPLETED_BY[key]);
  const unfinished = STAGES.find((stage) => stage.key !== "outcome" && !done(stage.key));
  const failedAt =
    run.status === "failed" ? (FAILS_AT[run.error_code] ?? unfinished?.key ?? "outcome") : null;

  let stopped = false;
  return STAGES.map((stage) => {
    let status;
    if (stage.key === "outcome") {
      status = { completed: "done", failed: "failed", running: "pending" }[run.status];
    } else if (stage.key === "validation" && failedAt === "emissions") {
      status = "done"; // the response passed validation; applying its emissions failed
    } else if (done(stage.key)) {
      status = "done";
    } else if (stopped) {
      status = run.status === "failed" ? "skipped" : "pending";
    } else if (run.status === "failed") {
      status = stage.key === failedAt ? "failed" : "skipped";
      stopped = stage.key === failedAt;
    } else {
      status = "running";
      stopped = true;
    }
    return { ...stage, status };
  });
}

/** One-line summaries shown under each pipeline stage. */
export function stageSummaries(detail) {
  const count = (list) => (list ?? []).length;
  const response = detail.response;
  const toolCount = count(detail.ir?.tools);
  return {
    message: excerpt(detail.message?.text ?? "", 42),
    carry: `${count(detail.carried)} carried`,
    jev: detail.classification
      ? `${count(detail.classification.classifications)} labels`
      : detail.failure?.rejected_stage === "jev"
        ? "output rejected"
        : "",
    context: detail.classification
      ? `+${count(detail.context_added)} −${count(detail.context_removed)} · ${count(detail.context)} active`
      : "",
    ir: detail.ir ? `${count(detail.ir.context)} nodes · ${toolCount} tools` : "",
    provider: `${detail.run.provider_id} / ${detail.run.model_id}`,
    validation: response
      ? "contract satisfied"
      : detail.failure?.rejected_stage === "response"
        ? "rejected"
        : "",
    emissions: response
      ? `+${count(detail.emitted_nodes)} nodes · +${count(detail.emitted_edges)} edges`
      : "",
    outcome: detail.run.status === "failed" ? detail.run.error_code : detail.run.status,
  };
}

/** A short, human label for a node, taken from its payload. */
export function nodeLabel(node) {
  const payload = node.payload ?? {};
  if (node.type === "conversation" && typeof payload.text === "string") {
    return `${payload.role === "assistant" ? "Assistant" : "User"}: ${payload.text}`;
  }
  for (const key of ["title", "text", "summary", "name", "description", "path"]) {
    const value = payload[key];
    if (typeof value === "string" && value.trim()) return value.trim();
  }
  const first = Object.values(payload).find((value) => typeof value === "string" && value.trim());
  return first ? first.trim() : "(no text)";
}

/** `text` shortened to `limit` characters with an ellipsis. */
export function excerpt(text, limit) {
  const flat = String(text).replace(/\s+/g, " ").trim();
  return flat.length <= limit ? flat : `${flat.slice(0, limit - 1).trimEnd()}…`;
}

/** Runs newest first, each marked `stale` when it is `running` but no server process executes it. */
export function runList(overview) {
  const executing = new Set(overview.executing ?? []);
  return [...overview.runs].reverse().map(({ run, message }) => ({
    id: run.id,
    status: run.status,
    errorCode: run.error_code,
    message: message ?? "",
    provider: run.provider_id,
    model: run.model_id,
    startedAt: run.started_at,
    stale: run.status === "running" && !executing.has(run.id),
  }));
}

/** Arc-diagram layout of the durable DAG: one row per node in creation order, one arc per edge. */
export function dagLayout(dag, { active = new Set(), emitted = new Set() } = {}) {
  const index = new Map(dag.nodes.map((node, position) => [node.id, position]));
  const rows = dag.nodes.map((node, position) => ({
    node,
    position,
    label: nodeLabel(node),
    active: active.has(node.id),
    emitted: emitted.has(node.id),
  }));
  const arcs = dag.edges
    .filter((edge) => index.has(edge.from_node_id) && index.has(edge.to_node_id))
    .map((edge) => {
      const from = index.get(edge.from_node_id);
      const to = index.get(edge.to_node_id);
      return { edge, from, to, span: Math.abs(from - to) };
    });
  return { rows, arcs, maxSpan: Math.max(0, ...arcs.map((arc) => arc.span)) };
}

/** Incoming and outgoing edges of `nodeId`, each with the node on the other end. */
export function neighbours(dag, nodeId) {
  const byId = new Map(dag.nodes.map((node) => [node.id, node]));
  const outgoing = dag.edges
    .filter((edge) => edge.from_node_id === nodeId)
    .map((edge) => ({ edge, node: byId.get(edge.to_node_id) }));
  const incoming = dag.edges
    .filter((edge) => edge.to_node_id === nodeId)
    .map((edge) => ({ edge, node: byId.get(edge.from_node_id) }));
  return { incoming, outgoing };
}

/**
 * The run's active context joined with its durable nodes, plus what Jev removed. Removed nodes are
 * listed with their node, which proves they are still durable.
 */
export function contextView(detail, dag) {
  const byId = new Map(dag.nodes.map((node) => [node.id, node]));
  const added = new Set(detail.context_added ?? []);
  const members = (detail.context ?? []).map((member) => ({
    ...member,
    node: byId.get(member.node_id),
    added: added.has(member.node_id),
  }));
  const removed = (detail.context_removed ?? []).map((nodeId) => ({
    nodeId,
    node: byId.get(nodeId),
  }));
  return { members, removed, carried: detail.carried ?? [] };
}

/**
 * Jev's request candidates with the label Jev gave each and what that did to membership:
 * `added`, `removed`, `kept`, `left out`, or `unlabeled` (carried unchanged).
 */
export function jevView(detail) {
  const labels = new Map(
    (detail.classification?.classifications ?? []).map((entry) => [entry.node_id, entry.classification]),
  );
  const added = new Set(detail.context_added ?? []);
  const removed = new Set(detail.context_removed ?? []);
  const rows = (detail.jev_request?.candidates ?? []).map((candidate) => {
    const label = labels.get(candidate.node_id) ?? null;
    let effect;
    if (added.has(candidate.node_id)) effect = "added";
    else if (removed.has(candidate.node_id)) effect = "removed";
    else if (label === "active") effect = "kept";
    else if (label === "inactive") effect = "left out";
    else effect = candidate.in_context ? "unlabeled (kept)" : "unlabeled";
    return { candidate, label, effect, labelText: nodeLabel({ type: candidate.type, payload: candidate.payload }) };
  });
  return { rows, labeled: labels.size, candidates: rows.length };
}

/** Emissions of a validated response, paired with the durable IDs they became. */
export function emissionView(detail) {
  const createdByRef = new Map();
  const createdEdges = [];
  for (const event of detail.events ?? []) {
    if (event.type === "dag.node_created") createdByRef.set(event.payload.ref, event.payload.node_id);
    if (event.type === "dag.edge_created") createdEdges.push(event.payload.edge_id);
  }
  let edgeIndex = 0;
  return (detail.response?.emissions ?? []).map((emission) =>
    emission.kind === "node"
      ? { emission, durableId: createdByRef.get(emission.ref) ?? null }
      : { emission, durableId: createdEdges[edgeIndex++] ?? null },
  );
}

/** Events in order, with each run of consecutive deltas folded into one group. */
export function eventGroups(events) {
  const groups = [];
  const start = events.length ? Date.parse(events[0].created_at) : 0;
  for (const event of events) {
    const offset = Date.parse(event.created_at) - start;
    const last = groups[groups.length - 1];
    if (event.type === "inference.delta" && last?.type === "inference.delta") {
      last.count += 1;
      last.text += event.payload.text;
      last.lastSequence = event.sequence;
      continue;
    }
    groups.push({
      type: event.type,
      sequence: event.sequence,
      lastSequence: event.sequence,
      offset,
      count: 1,
      text: event.type === "inference.delta" ? event.payload.text : "",
      event,
    });
  }
  return groups;
}

/** The category of an event type, for colour: run, context, jev, ir, inference, response, dag. */
export function eventCategory(type) {
  return type.split(".")[0];
}

/** Milliseconds between two timestamps as a compact duration, e.g. `1.2 s`. */
export function duration(startedAt, completedAt) {
  if (!startedAt || !completedAt) return "";
  const ms = Date.parse(completedAt) - Date.parse(startedAt);
  if (!Number.isFinite(ms) || ms < 0) return "";
  if (ms < 1000) return `${ms} ms`;
  if (ms < 60_000) return `${(ms / 1000).toFixed(1)} s`;
  return `${Math.floor(ms / 60_000)} min ${Math.round((ms % 60_000) / 1000)} s`;
}

/** Applies a streamed event to a live run record `{ runId, events, streamed }`, returning it. */
export function applyLiveEvent(live, event) {
  const next = live && live.runId === event.run_id ? live : { runId: event.run_id, events: [], streamed: "" };
  if (next.events.some((known) => known.sequence === event.sequence)) return next;
  next.events = [...next.events, event].sort((a, b) => a.sequence - b.sequence);
  if (event.type === "inference.delta") next.streamed += event.payload.text;
  return next;
}

/** Whether an event ends its run. */
export function isTerminal(event) {
  return event.type === "run.completed" || event.type === "run.failed";
}
