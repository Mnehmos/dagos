// The Lint view: every changed function judged against the project's plain-English rules, as a
// heatmap of functions × rules. Rendering is pure (state in, escaped HTML out) so it is tested
// under Node; app.js wires it to the API.

import { esc } from "./view.js";

/** Whether a person marked `rule` "not a problem here" for `fn`. */
export function isDismissed(report, rule, fn) {
  return (report.dismissed ?? []).some((d) => d.rule === rule && d.file === fn.file && d.function === fn.function);
}

/** Summary counts of a report. */
export function lintSummary(report) {
  const judgments = report.functions.length * report.rules.length;
  return { functions: report.functions.length, rules: report.rules.length, judgments, findings: report.findings.length };
}

function cellHtml(report, fn, rule, p, index) {
  const hit = p >= report.threshold;
  const dismissed = hit && isDismissed(report, rule.id, fn);
  const classes = ["cell", hit ? "hit" : "", dismissed ? "dismissed" : ""].filter(Boolean).join(" ");
  const label = `${rule.text} ${p.toFixed(2)}${dismissed ? " (not a problem here)" : ""}`;
  return `<td class="${classes}" style="--p:${p}" title="${esc(label)}" data-lint-row="${index}"><span class="sr-only">${esc(label)}</span></td>`;
}

/** The whole view: the form, the heatmap, and the selected function's detail. */
export function lintHtml({ report, loading = false, error = null, selected = null, files = "" }) {
  const form = `<form class="lint-form" data-form="lint-run">
      <label for="lint-files" class="sr-only">Files to lint</label>
      <input id="lint-files" name="files" value="${esc(files)}" placeholder="Files changed since the last commit, or paths separated by commas" autocomplete="off" spellcheck="false">
      <button type="submit" class="primary" ${loading ? "disabled" : ""}>${loading ? "Judging…" : "Lint"}</button>
    </form>`;
  const intro = `<p class="hint">Jev judges each function against every rule (one yes/no each, in parallel). Rules and hints are in Settings → Review loop; the same judgments review what runs change.</p>`;
  if (error) return `${form}<p class="callout callout-warn">${esc(error)}</p>${intro}`;
  if (!report) return `${form}${intro}`;
  if (!report.functions.length) {
    return `${form}<p class="empty-note">No functions to judge in ${report.files.length ? esc(report.files.join(", ")) : "the changed files"} (Rust, JavaScript, TypeScript, and Python are read).</p>`;
  }
  const summary = lintSummary(report);
  const head = report.rules
    .map((rule, index) => `<th scope="col" title="${esc(rule.text)}"><span aria-hidden="true">${index + 1}</span><span class="sr-only">${esc(rule.text)}</span></th>`)
    .join("");
  const rows = report.functions
    .map((fn, index) => {
      const cells = fn.scores.map((p, rule) => cellHtml(report, fn, report.rules[rule], p, index)).join("");
      const count = fn.scores.filter((p, rule) => p >= report.threshold && !isDismissed(report, report.rules[rule].id, fn)).length;
      return `<tr class="${index === selected ? "selected" : ""}">
        <th scope="row"><button type="button" class="lint-fn" data-lint-row="${index}" aria-pressed="${index === selected}">
          <code>${esc(fn.function)}</code><span class="lint-where">${esc(fn.file)}:${fn.line}</span>${count ? `<span class="lint-count">${count}</span>` : ""}
        </button></th>${cells}</tr>`;
    })
    .join("");
  const legend = report.rules.map((rule, index) => `<li><span class="lint-num">${index + 1}</span> ${esc(rule.text)}</li>`).join("");
  return `${form}
    <p class="lint-summary">${summary.functions} function${summary.functions === 1 ? "" : "s"} × ${summary.rules} rules = ${summary.judgments} judgments · <strong>${summary.findings} finding${summary.findings === 1 ? "" : "s"}</strong> at ${report.threshold} or above</p>
    <div class="lint-grid-wrap"><table class="lint-grid">
      <thead><tr><th scope="col">Function</th>${head}</tr></thead>
      <tbody>${rows}</tbody>
    </table></div>
    ${selected != null && report.functions[selected] ? functionHtml(report, selected) : `<p class="hint">Select a function to see its source and every judgment.</p>`}
    <details class="lint-legend"><summary>Rules</summary><ol>${legend}</ol></details>`;
}

/** One function: its source and each rule's probability, with dismissals for findings. */
export function functionHtml(report, index) {
  const fn = report.functions[index];
  const judged = report.rules
    .map((rule, position) => ({ rule, p: fn.scores[position] }))
    .sort((a, b) => b.p - a.p)
    .map(({ rule, p }) => {
      const hit = p >= report.threshold;
      const dismissed = hit && isDismissed(report, rule.id, fn);
      const action = hit
        ? `<button type="button" class="link" data-dismiss="${dismissed ? "restore" : "dismiss"}" data-rule="${esc(rule.id)}" data-index="${index}">${dismissed ? "Restore" : "Not a problem here"}</button>`
        : "";
      return `<li class="${hit ? (dismissed ? "dismissed" : "hit") : ""}"><span class="lint-p" style="--p:${p}">${p.toFixed(2)}</span> ${esc(rule.text)} ${action}</li>`;
    })
    .join("");
  return `<section class="lint-detail" aria-label="${esc(fn.function)}">
    <header><code>${esc(fn.function)}</code> <span class="lint-where">${esc(fn.file)}:${fn.line}</span></header>
    <ol class="lint-judgments">${judged}</ol>
    <pre class="lint-source"><code>${esc(fn.text)}</code></pre>
  </section>`;
}
