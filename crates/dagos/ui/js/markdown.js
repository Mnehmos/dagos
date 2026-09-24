// Markdown for replies and tool output: headings, paragraphs, nested and ordered lists,
// blockquotes, tables, horizontal rules, fenced code (also inside list items), and inline code,
// bold, italics, strikethrough, and http(s) links. Text is escaped before any markup is added, so
// model and tool output can never inject HTML. Pure: tested under Node.

import { esc } from "./view.js";

const FENCE = /^(\s*)(```+|~~~+)\s*([\w+#.-]*)\s*$/;
const HEADING = /^\s{0,3}(#{1,6})\s+(.*?)\s*#*\s*$/;
const RULE = /^\s{0,3}([-*_])(\s*\1){2,}\s*$/;
const QUOTE = /^\s{0,3}>/;
const ITEM = /^(\s*)([-*+]|\d{1,9}[.)])\s+(.*)$/;
const TABLE_ROW = /^\s*\|.*\|\s*$/;
const TABLE_RULE = /^\s*\|?\s*:?-{2,}:?\s*(\|\s*:?-{2,}:?\s*)*\|?\s*$/;

const indentOf = (line) => line.length - line.trimStart().length;

function inlineHtml(text) {
  return String(text)
    .split(/(`+[^`\n]*?`+)/)
    .map((part) => {
      const code = part.match(/^(`+)([^`\n]*?)\1$/);
      if (code) return `<code>${esc(code[2].trim() || code[2])}</code>`;
      return esc(part)
        .replace(/\*\*([^*\n]+?)\*\*/g, "<strong>$1</strong>")
        .replace(/(^|[^\w])__([^_\n]+?)__(?=$|[^\w])/g, "$1<strong>$2</strong>")
        .replace(/(^|[^*\w])\*([^*\n\s][^*\n]*?)\*(?=$|[^*\w])/g, "$1<em>$2</em>")
        .replace(/(^|[^\w])_([^_\n\s][^_\n]*?)_(?=$|[^\w])/g, "$1<em>$2</em>")
        .replace(/~~([^~\n]+?)~~/g, "<del>$1</del>")
        .replace(
          /\[([^\]\n]+)\]\((https?:\/\/[^\s)]+)\)/g,
          (_, label, url) => `<a href="${url}" target="_blank" rel="noopener noreferrer">${label}</a>`,
        );
    })
    .join("");
}

function startsBlock(line) {
  return FENCE.test(line) || HEADING.test(line) || RULE.test(line) || QUOTE.test(line) || ITEM.test(line) || TABLE_ROW.test(line);
}

/** `lines` with their common indentation removed. */
function dedent(lines) {
  const indents = lines.filter((line) => line.trim()).map(indentOf);
  const common = indents.length ? Math.min(...indents) : 0;
  return lines.map((line) => line.slice(Math.min(common, indentOf(line))));
}

function tableHtml(lines) {
  const cells = (line) => line.trim().replace(/^\|/, "").replace(/\|$/, "").split("|").map((cell) => cell.trim());
  const [head, , ...rows] = lines;
  const header = cells(head).map((cell) => `<th>${inlineHtml(cell)}</th>`).join("");
  const body = rows.map((row) => `<tr>${cells(row).map((cell) => `<td>${inlineHtml(cell)}</td>`).join("")}</tr>`).join("");
  return `<div class="md-table"><table><thead><tr>${header}</tr></thead><tbody>${body}</tbody></table></div>`;
}

/** A list starting at `start`: its HTML and the index of the first line after it. */
function listHtml(lines, start) {
  const first = lines[start].match(ITEM);
  const base = first[1].length;
  const ordered = /\d/.test(first[2]);
  const items = [];
  let index = start;
  while (index < lines.length) {
    const match = lines[index].match(ITEM);
    if (!match || match[1].length !== base || /\d/.test(match[2]) !== ordered) break;
    const body = [match[3]];
    index += 1;
    while (index < lines.length) {
      const line = lines[index];
      if (!line.trim()) {
        let next = index + 1;
        while (next < lines.length && !lines[next].trim()) next += 1;
        if (next < lines.length && indentOf(lines[next]) > base) {
          body.push("");
          index = next;
          continue;
        }
        break;
      }
      if (indentOf(line) <= base) break;
      body.push(line);
      index += 1;
    }
    const [lead, ...rest] = body;
    const content = rest.length ? blocksHtml([lead, ...dedent(rest)]) : inlineHtml(lead);
    items.push(`<li>${content}</li>`);
  }
  const tag = ordered ? "ol" : "ul";
  const number = ordered ? Number.parseInt(first[2], 10) : 1;
  return [`<${tag}${ordered && number !== 1 ? ` start="${number}"` : ""}>${items.join("")}</${tag}>`, index];
}

function blocksHtml(lines) {
  const out = [];
  let index = 0;
  while (index < lines.length) {
    const line = lines[index];
    if (!line.trim()) {
      index += 1;
      continue;
    }
    const fence = line.match(FENCE);
    if (fence) {
      const close = new RegExp(`^\\s*${fence[2][0] === "`" ? "`" : "~"}{${fence[2].length},}\\s*$`);
      const code = [];
      for (index += 1; index < lines.length && !close.test(lines[index]); index += 1) code.push(lines[index]);
      index += 1;
      const language = fence[3] ? ` data-lang="${esc(fence[3])}"` : "";
      out.push(`<pre class="code"${language}><code>${esc(dedent(code).join("\n").replace(/\s+$/, ""))}</code></pre>`);
      continue;
    }
    const heading = line.match(HEADING);
    if (heading) {
      const level = Math.min(6, heading[1].length + 2);
      out.push(`<h${level}>${inlineHtml(heading[2])}</h${level}>`);
      index += 1;
      continue;
    }
    if (RULE.test(line)) {
      out.push("<hr>");
      index += 1;
      continue;
    }
    if (QUOTE.test(line)) {
      const quoted = [];
      while (index < lines.length && QUOTE.test(lines[index])) {
        quoted.push(lines[index].replace(/^\s{0,3}>\s?/, ""));
        index += 1;
      }
      out.push(`<blockquote>${blocksHtml(quoted)}</blockquote>`);
      continue;
    }
    if (TABLE_ROW.test(line) && TABLE_RULE.test(lines[index + 1] ?? "")) {
      const rows = [];
      while (index < lines.length && TABLE_ROW.test(lines[index])) {
        rows.push(lines[index]);
        index += 1;
      }
      out.push(tableHtml(rows));
      continue;
    }
    if (ITEM.test(line)) {
      const [html, next] = listHtml(lines, index);
      out.push(html);
      index = next;
      continue;
    }
    const paragraph = [];
    while (index < lines.length && lines[index].trim() && (paragraph.length === 0 || !startsBlock(lines[index]))) {
      paragraph.push(lines[index].trim());
      index += 1;
    }
    out.push(`<p>${paragraph.map(inlineHtml).join("<br>")}</p>`);
  }
  return out.join("");
}

/** Renders markdown text as HTML. */
export function markdownHtml(text) {
  return blocksHtml(String(text ?? "").replace(/\r\n?/g, "\n").split("\n"));
}
