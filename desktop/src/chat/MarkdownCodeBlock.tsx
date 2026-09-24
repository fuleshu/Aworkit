import { isValidElement, type ComponentProps, type ReactNode } from "react";
import { useCopyFeedback } from "./clipboard";
import "./markdownCode.css";

/** What React Markdown hands the `pre` override as its single child. */
interface CodeElementProps {
  readonly children?: ReactNode;
  readonly className?: string;
}

/**
 * One fenced code block with a copy control in its header.
 *
 * The copied text is the block's source: React Markdown gives the `pre` its
 * `code` element, and the text is read back from that element rather than from
 * the rendered DOM, so wrapping or highlighting can never change what lands on
 * the clipboard. The language tag is shown when the provider wrote one.
 */
export function MarkdownCodeBlock({
  children,
  ...rest
}: ComponentProps<"pre">): React.JSX.Element {
  const code = codeElement(children);
  const source = textOf(code?.props.children ?? children);
  const language = languageOf(code?.props.className);
  const { copied, copy } = useCopyFeedback(source);
  return (
    <div className="markdown-code">
      <div className="markdown-code-header">
        {language !== null && (
          <span className="markdown-code-language">{language}</span>
        )}
        <button
          className="markdown-code-copy"
          type="button"
          aria-label={
            copied ? "Copied to the clipboard" : "Copy this code block"
          }
          title={
            copied
              ? "Copied to the clipboard"
              : "Copy this code block to the clipboard"
          }
          onClick={copy}
        >
          {copied ? "Copied" : "Copy"}
        </button>
      </div>
      <pre {...rest}>{children}</pre>
    </div>
  );
}

/** The `code` element inside a `pre`, or null when the block has another shape. */
function codeElement(
  children: ReactNode,
): { readonly props: CodeElementProps } | null {
  if (!isValidElement(children)) return null;
  const element = children as {
    readonly type?: unknown;
    readonly props: CodeElementProps;
  };
  return element.type === "code" ? { props: element.props } : null;
}

/** Concatenates the text of a React subtree, which is the block's source text. */
function textOf(node: ReactNode): string {
  if (typeof node === "string") return node;
  if (typeof node === "number") return String(node);
  if (Array.isArray(node)) return node.map(textOf).join("");
  if (isValidElement(node)) {
    return textOf((node.props as { readonly children?: ReactNode }).children);
  }
  return "";
}

/** The fence's language tag, read from the `language-*` class React Markdown adds. */
function languageOf(className: string | undefined): string | null {
  const match = /(?:^|\s)language-([\w+.-]+)/.exec(className ?? "");
  return match === null ? null : match[1];
}
