// Chat rendering under Node: `node --test crates/dagos/ui/tests`. Model replies are untrusted, so
// the markdown subset must never let markup through.

import assert from "node:assert/strict";
import { test } from "node:test";

import {
  chatHeaderHtml,
  conversationListHtml,
  markdownHtml,
  projectMenuHtml,
  relativeTime,
  reviewHtml,
  threadHtml,
} from "../js/chat.js";

test("markdown escapes everything it does not render", () => {
  const html = markdownHtml('<script>alert(1)</script> <img src=x onerror="x"> **bold** `<b>`');
  assert.ok(!html.includes("<script>"));
  assert.ok(!html.includes("<img"));
  assert.ok(html.includes("&lt;script&gt;"));
  assert.ok(html.includes("<strong>bold</strong>"));
  assert.ok(html.includes("<code>&lt;b&gt;</code>"), "code spans are escaped, not formatted");
});

test("markdown renders fences, headings, lists, and paragraphs", () => {
  const html = markdownHtml(["# Plan", "", "- one", "- **two**", "", "1. first", "", "```rust", 'fn x() { "<" }', "```", "", "done"].join("\n"));
  assert.ok(html.includes("<h3>Plan</h3>"));
  assert.ok(html.includes("<ul><li>one</li><li><strong>two</strong></li></ul>"));
  assert.ok(html.includes("<ol><li>first</li></ol>"));
  assert.ok(html.includes('<pre class="code" data-lang="rust"><code>fn x() { &quot;&lt;&quot; }</code></pre>'));
  assert.ok(html.endsWith("<p>done</p>"));
});

test("only http(s) links become anchors", () => {
  const html = markdownHtml("[ok](https://example.com/a) [bad](javascript:alert(1))");
  assert.ok(html.includes('<a href="https://example.com/a" target="_blank" rel="noopener noreferrer">ok</a>'));
  assert.ok(!html.includes('href="javascript'));
});

test("relative times read naturally", () => {
  const now = Date.parse("2026-09-22T12:00:00Z");
  assert.equal(relativeTime("2026-09-22T11:59:40Z", now), "just now");
  assert.equal(relativeTime("2026-09-22T11:55:00Z", now), "5m");
  assert.equal(relativeTime("2026-09-22T09:00:00Z", now), "3h");
  assert.equal(relativeTime("2026-09-20T12:00:00Z", now), "2d");
  assert.equal(relativeTime("not a time", now), "");
});

const conversation = (id, title, archived = null) => ({
  id,
  title,
  updated_at: "2026-09-22T12:00:00Z",
  archived_at: archived,
});

test("the conversation list separates archived chats and marks the open one", () => {
  const html = conversationListHtml(
    [conversation("conv_a", "Storage <plan>"), conversation("conv_b", "Old", "2026-09-21T00:00:00Z")],
    "conv_a",
  );
  assert.ok(html.includes('data-conversation="conv_a"') && html.includes('aria-current="true"'));
  assert.ok(html.includes("Storage &lt;plan&gt;"));
  assert.ok(html.includes("Archived (1)"));
  assert.ok(conversationListHtml([], null).includes("No chats yet"));
});

test("turns show the message, the reply, failures, and Jev fallbacks", () => {
  const run = (id, status, extra = {}) => ({ id, status, provider_id: "openrouter", model_id: "m", ...extra });
  const html = threadHtml(
    {
      turns: [
        { run: run("run_1", "completed"), message: "Hi", prose: "**Hello**", context_size: 2, jev_id: "openrouter-jev:~typesafe/jev-latest", jev_fallback: false, emitted_nodes: 1, emitted_edges: 0, recalled: 2, omitted_results: 1, full_context: true, failure: null },
        { run: run("run_2", "failed"), message: "Break", prose: "", context_size: 0, jev_id: "fake-jev", jev_fallback: true, emitted_nodes: 0, emitted_edges: 0, failure: { error_code: "response_invalid", message: "run failed", rejected_stage: "response", rejected_reason: "not JSON" } },
        { run: run("run_3", "running"), message: "Next", prose: "", context_size: 0, jev_id: null, jev_fallback: false, emitted_nodes: 0, emitted_edges: 0, failure: null },
      ],
    },
    { live: { run_3: "Stream<ing>" } },
  );
  assert.ok(html.includes("<strong>Hello</strong>"));
  assert.ok(html.includes("openrouter · ~typesafe/jev-latest"));
  assert.ok(html.includes("+1 node"));
  assert.ok(html.includes("recalled 2") && html.includes("1 result left out"));
  assert.equal((html.match(/>full context</g) ?? []).length, 1, "only the turn that loaded everything says so");
  assert.equal((html.match(/recalled \d/g) ?? []).length, 1, "only turns that recalled say so");
  assert.ok(html.includes("The reply was rejected") && html.includes("not JSON"));
  assert.ok(html.includes("Jev fallback"));
  assert.ok(html.includes('data-live-run="run_3">Stream&lt;ing&gt;'), "streamed prose is plain, escaped text");
  assert.equal((html.match(/data-inspect=/g) ?? []).length, 2, "finished turns can be inspected");
});

test("reviews show findings, clean results, and errors in the turn", () => {
  const run = { id: "run_9", status: "completed", provider_id: "openrouter", model_id: "m" };
  const finding = { rule: "swallows-errors", text: "Swallows <errors>.", file: "src/lib.rs", function: "load", line: 5, probability: 0.934 };
  const html = threadHtml({
    turns: [{
      run, message: "Fix it", prose: "", context_size: 0, jev_id: null, jev_fallback: false, emitted_nodes: 0, emitted_edges: 0, failure: null,
      items: [
        { kind: "prose", text: "Done." },
        { kind: "review", round: 1, judged: 2, findings: [finding], error: null },
        { kind: "prose", text: "Fixed the finding." },
        { kind: "review", round: 2, judged: 1, findings: [], error: null },
      ],
    }],
  });
  assert.ok(html.includes("Review 1: 1 finding in 2 changed functions"));
  assert.ok(html.includes("0.93") && html.includes("src/lib.rs:5") && html.includes("Swallows &lt;errors&gt;."));
  assert.ok(html.includes("Review 2: 1 changed function judged, no rule applies"));
  assert.ok(html.indexOf("Done.") < html.indexOf("Review 1") && html.indexOf("Review 1") < html.indexOf("Fixed the finding."), "in order");
  assert.ok(reviewHtml({ round: 1, judged: 0, findings: [], error: "Jev <down>" }, "run_9").includes("could not run: Jev &lt;down&gt;"));
});

test("headers and menus escape names", () => {
  const header = chatHeaderHtml(conversation("conv_a", "<b>x</b>"));
  assert.ok(header.includes("&lt;b&gt;x&lt;/b&gt;") && header.includes('data-action="archive"'));
  assert.ok(chatHeaderHtml(conversation("conv_a", "t", "2026-01-01T00:00:00Z")).includes('data-action="restore"'));
  assert.ok(chatHeaderHtml(null).includes("New chat"));
  const menu = projectMenuHtml([{ project: { id: "proj_1", name: "<Demo>" }, conversations: 1, nodes: 3 }], "proj_1");
  assert.ok(menu.includes("&lt;Demo&gt;") && menu.includes("1 chat · 3 nodes"));
});
