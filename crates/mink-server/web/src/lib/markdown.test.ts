import { describe, it, expect } from "vitest";
import { renderMarkdown, sanitizeHtml } from "./markdown";

describe("markdown XSS 防护", () => {
  it("剥离 script 标签", () => {
    const out = sanitizeHtml("<p>hi</p><script>alert(1)</script>");
    expect(out).not.toContain("<script");
    expect(out).toContain("alert(1)"); // 文本保留
  });

  it("剥离事件属性", () => {
    const out = sanitizeHtml('<img src="x.png" onerror="alert(1)">');
    expect(out).not.toContain("onerror");
  });

  it("剥离 javascript: 链接", () => {
    const out = sanitizeHtml('<a href="javascript:alert(1)">x</a>');
    expect(out).not.toContain("javascript:");
  });

  it("保留正常 markdown 结构与安全链接", () => {
    const out = sanitizeHtml('<p><strong>b</strong> <a href="/file">f</a></p>');
    expect(out).toContain("<strong>b</strong>");
    expect(out).toContain('href="/file"');
  });

  it("renderMarkdown 全链路清洗", () => {
    const out = renderMarkdown("**ok** <script>alert(1)</script>");
    expect(out).toContain("<strong>ok</strong>");
    expect(out).not.toContain("<script");
  });
});
