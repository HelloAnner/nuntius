/* Markdown renderer with framed, copyable code blocks. */
import { memo, useState, type ReactNode } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import rehypeHighlight from "rehype-highlight";
import { IconCheck, IconCopy } from "./icons";

function CodeFrame({ lang, children }: { lang: string; children: ReactNode }) {
  const [copied, setCopied] = useState(false);
  const copy = () => {
    const text = extractText(children);
    void navigator.clipboard?.writeText(text).catch(() => {});
    setCopied(true);
    setTimeout(() => setCopied(false), 1600);
  };
  return (
    <div className="codeblock">
      <div className="cb-head">
        <span className="cb-lang">{lang || "text"}</span>
        <button className={`cb-copy${copied ? " done" : ""}`} onClick={copy}>
          {copied ? <IconCheck size={13} /> : <IconCopy size={13} />}
          {copied ? "已复制" : "复制"}
        </button>
      </div>
      <pre>{children}</pre>
    </div>
  );
}

function extractText(node: ReactNode): string {
  if (node == null || typeof node === "boolean") return "";
  if (typeof node === "string" || typeof node === "number") return String(node);
  if (Array.isArray(node)) return node.map(extractText).join("");
  if (typeof node === "object" && "props" in node) {
    return extractText((node as { props: { children?: ReactNode } }).props.children);
  }
  return "";
}

export const Markdown = memo(function Markdown({ text }: { text: string }) {
  return (
    <div className="md">
      <ReactMarkdown
        remarkPlugins={[remarkGfm]}
        rehypePlugins={[[rehypeHighlight, { detect: false, ignoreMissing: true }]]}
        components={{
          pre({ children }) {
            return <>{children}</>;
          },
          code({ className, children, ...rest }) {
            const match = /language-([\w+-]+)/.exec(className ?? "");
            const isBlock = Boolean(match) || String(children).includes("\n");
            if (isBlock) {
              return (
                <CodeFrame lang={match?.[1] ?? ""}>
                  <code className={className} {...rest}>
                    {children}
                  </code>
                </CodeFrame>
              );
            }
            return (
              <code className={className} {...rest}>
                {children}
              </code>
            );
          },
          a({ href, children }) {
            return (
              <a href={href} target="_blank" rel="noreferrer noopener">
                {children}
              </a>
            );
          },
        }}
      >
        {text}
      </ReactMarkdown>
    </div>
  );
});
