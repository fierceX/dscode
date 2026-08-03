// marked 封装：流式文本渲染（消息级重渲染，异常时回退纯文本）。
// XSS 防护：marked 输出经 DOMParser 白名单清洗后再 v-html——
// 剥离 script/iframe/事件属性/javascript: 链接（AI 输出受控但不可信）。

import { marked } from "marked";

/** 允许的 HTML 标签（markdown 常见子集 + 表格） */
const ALLOWED_TAGS = new Set([
  "p", "br", "hr", "h1", "h2", "h3", "h4", "h5", "h6",
  "ul", "ol", "li", "strong", "em", "b", "i", "del", "sup", "sub",
  "code", "pre", "blockquote", "a", "img",
  "table", "thead", "tbody", "tr", "th", "td",
  "span", "div", "input",
]);

/** 允许的属性（白名单，防事件注入/伪协议） */
const ALLOWED_ATTRS: Record<string, Set<string>> = {
  a: new Set(["href", "title", "target", "rel"]),
  img: new Set(["src", "alt", "title"]),
  code: new Set(["class"]),
  pre: new Set(["class"]),
  th: new Set(["align"]),
  td: new Set(["align"]),
  input: new Set(["type", "checked", "disabled"]),
  span: new Set(["class"]),
  div: new Set(["class"]),
};

/** 危险协议（javascript:/data: 等） */
const BAD_PROTO = /^\s*(javascript|data|vbscript)\s*:/i;

function isSafeUrl(url: string): boolean {
  const trimmed = url.trim();
  if (trimmed.startsWith("#") || trimmed.startsWith("/")) return true;
  return !BAD_PROTO.test(trimmed);
}

export function sanitizeHtml(html: string): string {
  if (typeof DOMParser === "undefined") return html; // 非浏览器环境（SSR）跳过
  const doc = new DOMParser().parseFromString(html, "text/html");
  const walk = (node: Element) => {
    for (const child of [...node.children]) {
      const tag = child.tagName.toLowerCase();
      if (!ALLOWED_TAGS.has(tag)) {
        // 不允许的标签：保留其文本内容（剥离包裹），删除元素本身
        child.replaceWith(...child.childNodes);
        continue;
      }
      // 属性白名单
      for (const attr of [...child.attributes]) {
        const name = attr.name.toLowerCase();
        const allowed = ALLOWED_ATTRS[tag]?.has(name) ?? false;
        if (!allowed || name.startsWith("on")) {
          child.removeAttribute(attr.name);
          continue;
        }
        // URL 属性防伪协议
        if ((name === "href" || name === "src") && !isSafeUrl(attr.value)) {
          child.removeAttribute(attr.name);
        }
      }
      // 相对链接安全化（纯展示，不强制补全）
      if (tag === "a" && child.getAttribute("href")) {
        child.setAttribute("rel", "nofollow noopener");
      }
      walk(child);
    }
  };
  walk(doc.body);
  return doc.body.innerHTML;
}

export function renderMarkdown(text: string): string {
  try {
    const raw = marked.parse(text || "", { breaks: true }) as string;
    return sanitizeHtml(raw);
  } catch {
    const div = document.createElement("div");
    div.textContent = text;
    return div.innerHTML;
  }
}
