// Settings panes for the review loop and the tool guard: pure renderers, escaped output.

import assert from "node:assert/strict";
import { test } from "node:test";

import { guardHtml, reviewHtml, ruleId, safetyNavHtml } from "../js/settings.js";

test("the safety entries show whether each is on", () => {
  const html = safetyNavHtml({ config: { enabled: false } }, { config: { guard: { enabled: true, threshold: 0.5 } } }, "guard");
  assert.ok(html.includes("Review loop") && html.includes("Tool guard"));
  assert.ok(/data-settings-select="review"[\s\S]*?Off/.test(html), "review is off");
  assert.ok(/data-settings-select="guard" aria-current="true"[\s\S]*?On/.test(html), "guard is on and selected");
});

test("rules render with their hints, escaped, and get IDs from their text", () => {
  const lint = { config: { enabled: true, threshold: 0.7, max_rounds: 3 }, file: "C:/p/.dagos/lint.json" };
  const html = reviewHtml(lint, [{ id: "x", text: "Swallows <errors>.", applies: "a & b", except: null }]);
  assert.ok(html.includes('value="Swallows &lt;errors&gt;."'));
  assert.ok(html.includes(">a &amp; b</textarea>"));
  assert.ok(html.includes('value="0.7"') && html.includes('value="3"') && html.includes("Rules (1)"));
  assert.equal(ruleId("Swallows errors."), "swallows-errors");
  assert.equal(ruleId("  Uses `unwrap()` in library code!  "), "uses-unwrap-in-library-code");
});

test("the guard pane lists its risks and marks the ones that always ask", () => {
  const tools = {
    config: { guard: { enabled: false, threshold: 0.4 } },
    guard_risks: [
      { id: "deletes-data", meaning: "Deletes <files>.", always_asks: true },
      { id: "uses-network", meaning: "Uses the network.", always_asks: false },
    ],
  };
  const html = guardHtml(tools);
  assert.ok(html.includes("Deletes &lt;files&gt;.") && html.includes('value="0.4"'));
  assert.equal((html.match(/always asks<\/span>/g) ?? []).length, 1);
  assert.ok(!html.includes('name="enabled" checked'), "off stays off");
});
