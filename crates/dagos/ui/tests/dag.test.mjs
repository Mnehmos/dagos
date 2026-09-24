// The DAG panel: chat messages are hidden unless asked for; what runs recorded stays in view.

import assert from "node:assert/strict";
import { test } from "node:test";

import { dagHtml } from "../js/view.js";

const node = (id, type, payload) => ({ id, type, payload, created_at: "2026-09-24T00:00:00.000Z", updated_at: "2026-09-24T00:00:00.000Z" });
const dag = {
  nodes: [
    node("node_1", "conversation", { role: "user", text: "Plan the store" }),
    node("node_2", "decision", { text: "Use SQLite" }),
    node("node_3", "conversation", { role: "user", text: "Add tests" }),
  ],
  edges: [{ id: "edge_1", from_node_id: "node_2", to_node_id: "node_1", type: "observed_from", created_at: "2026-09-24T00:00:00.000Z" }],
};
const base = { dag, active: new Set(), emitted: new Set(), selectedNodeId: null, runId: null, memberSources: new Map() };

test("messages are hidden by default and one toggle shows them", () => {
  const hidden = dagHtml(base);
  assert.ok(hidden.includes('data-node="node_2"'));
  assert.ok(!hidden.includes('data-node="node_1"') && !hidden.includes('data-node="node_3"'));
  assert.ok(hidden.includes("Show 2 messages") && hidden.includes('aria-pressed="false"'));
  assert.ok(hidden.includes("3 nodes · 1 edges"), "the counts are the whole DAG");
  const shown = dagHtml({ ...base, showMessages: true });
  assert.ok(shown.includes('data-node="node_1"') && shown.includes("Hide 2 messages"));
});

test("a selected message stays visible, and a DAG of only messages says so", () => {
  assert.ok(dagHtml({ ...base, selectedNodeId: "node_3" }).includes('data-node="node_3"'));
  const onlyMessages = { nodes: [dag.nodes[0]], edges: [] };
  const html = dagHtml({ ...base, dag: onlyMessages });
  assert.ok(html.includes("Only chat messages so far") && html.includes("Show 1 message<"));
});
