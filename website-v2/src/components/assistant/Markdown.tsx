import { Fragment, useMemo, useState, type ReactNode } from "react";
import { Check, Copy } from "lucide-react";
import { parseBlocks, parseInline, type Block, type Inline } from "./md-parse";

interface Labels {
  copy: string;
  copied: string;
}

function renderInline(nodes: Inline[]): ReactNode[] {
  return nodes.map((node, i) => {
    switch (node.kind) {
      case "text":
        return node.text;
      case "code":
        return <code key={i}>{node.text}</code>;
      case "strong":
        return <strong key={i}>{renderInline(node.children)}</strong>;
      case "em":
        return <em key={i}>{renderInline(node.children)}</em>;
      case "del":
        return <del key={i}>{renderInline(node.children)}</del>;
      case "link": {
        const external = !node.href.startsWith("/") && !node.href.startsWith("#");
        return (
          <a key={i} href={node.href} {...(external ? { target: "_blank", rel: "noopener noreferrer nofollow" } : {})}>
            {renderInline(node.children)}
          </a>
        );
      }
    }
  });
}

const inline = (text: string) => renderInline(parseInline(text));

function CodeBlock({ lang, text, labels }: { lang: string; text: string; labels: Labels }) {
  const [copied, setCopied] = useState(false);
  const copy = () => {
    navigator.clipboard.writeText(text).then(
      () => {
        setCopied(true);
        setTimeout(() => setCopied(false), 1500);
      },
      () => setCopied(false),
    );
  };
  return (
    <div className="ai-code">
      <div className="ai-code-bar">
        <span>{lang}</span>
        <button type="button" onClick={copy} aria-label={copied ? labels.copied : labels.copy} title={copied ? labels.copied : labels.copy}>
          {copied ? <Check size={13} aria-hidden /> : <Copy size={13} aria-hidden />}
        </button>
      </div>
      <pre>
        <code>{text}</code>
      </pre>
    </div>
  );
}

function renderBlocks(blocks: Block[], labels: Labels): ReactNode[] {
  return blocks.map((block, i) => {
    switch (block.kind) {
      case "code":
        return <CodeBlock key={i} lang={block.lang} text={block.text} labels={labels} />;
      case "heading": {
        const Tag = block.level <= 2 ? "h3" : "h4";
        return <Tag key={i}>{inline(block.text)}</Tag>;
      }
      case "hr":
        return <hr key={i} />;
      case "quote":
        return <blockquote key={i}>{renderBlocks(block.blocks, labels)}</blockquote>;
      case "list": {
        const items = block.items.map((item, j) => {
          const only = item.length === 1 ? item[0] : undefined;
          return <li key={j}>{only?.kind === "para" ? paraLines(only.lines) : renderBlocks(item, labels)}</li>;
        });
        return block.ordered ? (
          <ol key={i} start={block.start === 1 ? undefined : block.start}>
            {items}
          </ol>
        ) : (
          <ul key={i}>{items}</ul>
        );
      }
      case "table":
        return (
          <table key={i}>
            <thead>
              <tr>
                {block.head.map((cell, j) => (
                  <th key={j}>{inline(cell)}</th>
                ))}
              </tr>
            </thead>
            <tbody>
              {block.rows.map((row, j) => (
                <tr key={j}>
                  {block.head.map((_, k) => (
                    <td key={k}>{inline(row[k] ?? "")}</td>
                  ))}
                </tr>
              ))}
            </tbody>
          </table>
        );
      case "para":
        return <p key={i}>{paraLines(block.lines)}</p>;
    }
  });
}

/** 段内换行保留为 `<br>`（聊天回答里的换行通常有意义）。 */
function paraLines(lines: string[]): ReactNode[] {
  return lines.map((line, i) => (
    <Fragment key={i}>
      {i > 0 && <br />}
      {inline(line)}
    </Fragment>
  ));
}

/** 渲染 AI 回答的 Markdown（只输出 React 元素，不注入 HTML）。 */
export default function Markdown({ text, labels }: { text: string; labels: Labels }) {
  const blocks = useMemo(() => parseBlocks(text), [text]);
  return <div className="prose ai-md">{renderBlocks(blocks, labels)}</div>;
}
