import ReactMarkdown, { defaultUrlTransform } from "react-markdown";
import remarkGfm from "remark-gfm";

const renderedElements = [
  "p",
  "strong",
  "em",
  "del",
  "ul",
  "ol",
  "li",
  "a",
  "code",
  "pre",
  "blockquote",
  "h1",
  "h2",
  "h3",
  "h4",
  "hr",
  "br",
  "table",
  "thead",
  "tbody",
  "tr",
  "th",
  "td",
] as const;

interface MarkdownContentProps {
  readonly children: string;
  readonly className?: string;
}

/**
 * Renders provider-authored CommonMark and GFM without enabling raw HTML or
 * remote images. React Markdown's URL transform rejects unsafe protocols;
 * model citations open separately so they cannot replace the desktop shell.
 */
export function MarkdownContent({
  children,
  className,
}: MarkdownContentProps): React.JSX.Element {
  return (
    <div className={className}>
      <ReactMarkdown
        allowedElements={[...renderedElements]}
        components={{
          a: ({ node: _node, ...properties }) => (
            <a {...properties} rel="noreferrer" target="_blank" />
          ),
        }}
        remarkPlugins={[remarkGfm]}
        unwrapDisallowed
        urlTransform={defaultUrlTransform}
      >
        {children}
      </ReactMarkdown>
    </div>
  );
}
