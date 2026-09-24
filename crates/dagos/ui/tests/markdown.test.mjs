// The markdown renderer used for replies and tool output.

import assert from "node:assert/strict";
import { test } from "node:test";

import { toolOutputHtml } from "../js/chat.js";
import { markdownHtml } from "../js/markdown.js";

const lines = (...rows) => rows.join("\n");

test("nested lists follow indentation", () => {
  const html = markdownHtml(lines("- **Item 1**", "  - **Pid:** 8260", "  - **Name:** node", "- **Item 2**"));
  assert.equal(html, "<ul><li><p><strong>Item 1</strong></p><ul><li><strong>Pid:</strong> 8260</li><li><strong>Name:</strong> node</li></ul></li><li><strong>Item 2</strong></li></ul>");
});

test("ordered lists keep their start number", () => {
  assert.equal(markdownHtml(lines("3. c", "4. d")), '<ol start="3"><li>c</li><li>d</li></ol>');
});

test("blockquotes, rules, tables, italics, and strikethrough", () => {
  const html = markdownHtml(lines(
    "> **TOOL EXECUTION ERROR:** it broke",
    "> again",
    "",
    "---",
    "",
    "| Tool | Result |",
    "| --- | :---: |",
    "| `read_file` | _ok_ |",
    "",
    "~~old~~ *new* snake_case_name stays",
  ));
  assert.ok(html.startsWith("<blockquote><p><strong>TOOL EXECUTION ERROR:</strong> it broke<br>again</p></blockquote><hr>"));
  assert.ok(html.includes("<thead><tr><th>Tool</th><th>Result</th></tr></thead>"));
  assert.ok(html.includes("<td><code>read_file</code></td><td><em>ok</em></td>"));
  assert.ok(html.includes("<del>old</del> <em>new</em> snake_case_name stays"), html);
});

test("fences keep their content verbatim, also inside list items", () => {
  const html = markdownHtml(lines("- ```", "  OODA exec_cli smoke test", "  ```", "- after"));
  assert.ok(html.includes('<pre class="code"><code>OODA exec_cli smoke test</code></pre>'), html);
  assert.ok(html.includes("<li>after</li>"));
  const fenced = markdownHtml(lines("```js", "if (a < b) { **not bold** }", "```"));
  assert.equal(fenced, '<pre class="code" data-lang="js"><code>if (a &lt; b) { **not bold** }</code></pre>');
});

test("markup never gets through, even inside formatting", () => {
  const html = markdownHtml('**<img src=x onerror=alert(1)>** > <script>x</script> [a](javascript:alert(1))');
  assert.ok(!/<img|<script|href="javascript/.test(html), html);
});

test("OODA's markdown output renders as structure, without repeating the tool's name", () => {
  const output = { content: [{ type: "text", text: lines(
    "## `get_system_info` · ✅ Completed",
    "",
    "### Platform",
    "",
    "win32",
    "",
    "### Cpu",
    "",
    "- **Model:** `AMD Ryzen 5`",
    "- **Cores:** 12",
  ) }] };
  const html = toolOutputHtml({ name: "ooda.get_system_info", output });
  assert.ok(!html.includes("Completed"), "the leading heading repeats the card header");
  assert.ok(html.includes("<h5>Platform</h5><p>win32</p>"));
  assert.ok(html.includes("<li><strong>Model:</strong> <code>AMD Ryzen 5</code></li>"));
  const structured = toolOutputHtml({ name: "x.y", output: { content: [], structured: { ok: true } } });
  assert.ok(structured.startsWith('<pre class="json">'));
});
