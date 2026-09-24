// Tool calls in the chat and the Tools settings under Node. Tool output and server names come
// from outside DAGOS, so everything must be escaped.

import assert from "node:assert/strict";
import { test } from "node:test";

import { toolCardHtml, toolOutputText, turnHtml } from "../js/chat.js";
import { newToolServerHtml, toolServerHtml, toolServerStatus, toolsNavHtml } from "../js/settings.js";

const call = (extra) => ({
  call_id: "call_1",
  name: "ooda.exec_cli",
  arguments: { command: "dir <b>" },
  status: "awaiting",
  pending: false,
  decided_by: null,
  reason: null,
  output: null,
  ...extra,
});

test("only calls waiting for a person get approval buttons", () => {
  const waiting = toolCardHtml(call({ pending: true }), "run_1");
  assert.ok(!waiting.includes("approval-why"), "no reason beyond the policy, none shown");
  const flagged = toolCardHtml(call({ pending: true, pending_note: "Jev flagged it: destroys-data 0.93 <x>" }), "run_1");
  assert.ok(flagged.includes('class="approval-why">Jev flagged it: destroys-data 0.93 &lt;x&gt;'));
  assert.ok(flagged.includes('data-approve="always"'), "named-tool escalations can be remembered");
  const guarded = toolCardHtml(call({ pending: true, pending_guarded: true, pending_note: "Jev flagged it" }), "run_1");
  assert.ok(!guarded.includes('data-approve="always"'), "a guard flag asks every time: no Always allow");
  assert.ok(guarded.includes('data-approve="allow"') && guarded.includes('data-approve="deny"'));
  for (const decision of ["allow", "always", "deny"]) {
    assert.ok(waiting.includes(`data-approve="${decision}" data-run="run_1" data-call="call_1"`), decision);
  }
  assert.ok(waiting.includes("needs approval") && waiting.includes("<details open>"));
  assert.ok(waiting.includes("dir &lt;b&gt;"), "arguments are escaped");
  assert.ok(!toolCardHtml(call(), "run_1").includes("data-approve"), "policy checks show no buttons");
  const denied = toolCardHtml(call({ status: "denied", reason: "the user denied it" }), "run_1");
  assert.ok(denied.includes("denied") && denied.includes("the user denied it") && !denied.includes("data-approve"));
});

test("tool output reads as text, errors, notes, or JSON", () => {
  assert.equal(toolOutputText({ content: [{ type: "text", text: "a" }, { type: "text", text: "b" }] }), ["a", "b"].join(String.fromCharCode(10, 10)), "parts are separate blocks");
  assert.equal(toolOutputText({ error: "server crashed" }), "server crashed");
  assert.equal(toolOutputText({ content: [{ type: "image", note: "image data is not passed to models" }] }), "image data is not passed to models");
  assert.equal(toolOutputText({ content: [], structured: { ok: true } }), '{\n  "ok": true\n}');
  const card = toolCardHtml(call({ status: "completed", output: { content: [{ type: "text", text: "<script>" }] } }), "run_1");
  assert.ok(card.includes("&lt;script&gt;") && !card.includes("<script>"));
});

test("turns interleave replies and tool calls and hide stale streams while waiting", () => {
  const turn = {
    run: { id: "run_1", status: "running", provider_id: "fake", model_id: "fake-tool" },
    message: "Run it",
    prose: "Calling.",
    streaming: "",
    items: [{ kind: "prose", text: "Calling." }, { kind: "tool", ...call({ pending: true }) }],
  };
  const html = turnHtml(turn, { live: "Calling." });
  assert.ok(html.indexOf("Calling.") < html.indexOf("tool-call"), "prose, then the call");
  assert.ok(!html.includes("prose-live") && !html.includes("typing"), "nothing streams while a person decides");
});

const status = { id: "ooda", enabled: true, error: null, tools: [
  { name: "exec_cli", description: "Execute <shell> commands", policy: "ask" },
  { name: "mouse_click", description: "Click", policy: "off" },
] };
const server = { id: "ooda", command: "node", args: ["dist/index.js"], cwd: "C:/OODA", enabled: true };

test("server status pills say what models are offered", () => {
  assert.deepEqual(toolServerStatus(status), { kind: "ready", text: "1/2 tools" });
  assert.equal(toolServerStatus({ ...status, error: "no answer" }).text, "Error");
  assert.equal(toolServerStatus({ ...status, enabled: false }).text, "Off");
  const nav = toolsNavHtml({ config: { servers: [server] }, capabilities: { servers: [status] } }, "tool:ooda");
  assert.ok(nav.includes('data-settings-select="tool:ooda"') && nav.includes("1/2 tools") && nav.includes("+ Add MCP server"));
});

test("the server pane lists every tool with its policy and filters them", () => {
  const html = toolServerHtml(server, status, { filter: "click" });
  assert.ok(html.includes("1 of 2 offered to models"));
  assert.ok(html.includes("Execute &lt;shell&gt; commands"));
  assert.ok(/data-tool="exec_cli" data-policy="ask" aria-pressed="true"/.test(html));
  assert.ok(/data-tool="mouse_click" data-policy="off" aria-pressed="true"/.test(html));
  const rows = html.match(/<tr data-tool-row="[^"]*"( hidden)?>/g);
  assert.deepEqual(rows.map((row) => row.includes("hidden")), [true, false], "the filter hides non-matching tools");
  assert.ok(toolServerHtml(server, { ...status, error: "no answer within 30s" }).includes("no answer within 30s"));
});

test("imports offer Claude Desktop servers and flag the ones needing environment variables", () => {
  const html = newToolServerHtml({ servers: [
    { id: "ooda-computer", name: "ooda-computer", command: "node", args: ["x.js"], cwd: null, needs_env: false, added: false },
    { id: "github", name: "github", command: "npx", args: [], cwd: null, needs_env: true, added: true },
  ] });
  assert.ok(html.includes('data-settings-action="import-tool-server" data-index="0"'));
  assert.ok(html.includes("Added") && html.includes("does not copy"));
  assert.ok(!newToolServerHtml(null).includes("From Claude Desktop"));
});

test("the Jev tab shows each offered tool, its label, and whether the model saw it", async () => {
  const { jevToolsHtml } = await import("../js/view.js");
  const detail = {
    jev_request: { tools: [{ name: "ooda.read_file", description: "" }, { name: "ooda.mouse_click", description: "" }, { name: "ooda.exec_cli", description: "" }] },
    classification: { tools: [{ name: "ooda.read_file", classification: "active" }, { name: "ooda.mouse_click", classification: "inactive" }] },
    ir: { tools: [{ name: "ooda.read_file" }, { name: "ooda.exec_cli" }] },
  };
  const html = jevToolsHtml(detail);
  assert.ok(html.includes("2 of 3 exposed"));
  assert.ok(/tool-hidden">\s*<td class="grow"><code>ooda.mouse_click/.test(html));
  assert.ok(html.includes("label-inactive") && html.includes("hidden"));
  assert.equal(jevToolsHtml({ jev_request: { tools: [] } }), "");
});
