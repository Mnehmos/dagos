// The Lint view: heatmap of functions × rules, the selected function's judgments, dismissals.

import assert from "node:assert/strict";
import { test } from "node:test";

import { functionHtml, isDismissed, lintHtml, lintSummary } from "../js/lint.js";

const report = {
  files: ["src/lib.rs"],
  threshold: 0.7,
  rules: [
    { id: "swallows-errors", text: "Swallows <errors>." },
    { id: "dead-code", text: "Contains dead code." },
  ],
  functions: [
    { file: "src/lib.rs", function: "load", line: 5, text: "fn load() { read().ok(); }", scores: [0.94, 0.1] },
    { file: "src/lib.rs", function: "keep", line: 1, text: "fn keep() -> u8 { 1 }", scores: [0.72, 0.8] },
  ],
  findings: [{ rule: "swallows-errors", file: "src/lib.rs", function: "load", line: 5, probability: 0.94, text: "Swallows <errors>." }],
  dismissed: [{ rule: "swallows-errors", file: "src/lib.rs", function: "keep" }],
};

test("the heatmap shows every judgment, marks findings, and counts them", () => {
  assert.deepEqual(lintSummary(report), { functions: 2, rules: 2, judgments: 4, findings: 1 });
  const html = lintHtml({ report, selected: 0 });
  assert.equal((html.match(/<td class="cell/g) ?? []).length, 4, "one cell per judgment");
  assert.equal((html.match(/class="cell hit"/g) ?? []).length, 2, "load/swallows and keep/dead-code");
  assert.equal((html.match(/class="cell hit dismissed"/g) ?? []).length, 1);
  assert.ok(html.includes('title="Swallows &lt;errors&gt;. 0.94"'), "escaped, with the probability");
  assert.ok(html.includes("2 functions × 2 rules = 4 judgments"));
});

test("a function's detail lists judgments by probability with dismiss and restore", () => {
  assert.ok(isDismissed(report, "swallows-errors", report.functions[1]));
  const load = functionHtml(report, 0);
  assert.ok(load.indexOf("0.94") < load.indexOf("0.10"), "most probable first");
  assert.ok(load.includes('data-dismiss="dismiss" data-rule="swallows-errors" data-index="0"'));
  assert.ok(load.includes("fn load() { read().ok(); }"));
  const keep = functionHtml(report, 1);
  assert.ok(keep.includes('data-dismiss="restore" data-rule="swallows-errors"'));
  assert.ok(keep.includes('data-dismiss="dismiss" data-rule="dead-code"'));
});

test("empty and failed runs explain themselves", () => {
  assert.ok(lintHtml({ report: null }).includes('data-form="lint-run"'));
  assert.ok(lintHtml({ report: null, error: "needs a <Jev>" }).includes("needs a &lt;Jev&gt;"));
  assert.ok(lintHtml({ report: { ...report, functions: [] } }).includes("No functions to judge"));
  assert.ok(lintHtml({ report: null, loading: true }).includes("Judging…"));
});
