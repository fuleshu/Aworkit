import {
  useMemo,
  type ComponentPropsWithoutRef,
  type ReactNode,
} from "react";
import ReactMarkdown, { defaultUrlTransform, type ExtraProps } from "react-markdown";
import remarkGfm from "remark-gfm";
import {
  DOCUMENTS_FOLDER_HREF,
  resolveUserHelpTarget,
  slugifyHeading,
  type UserHelpTarget,
} from "./userHelp";

/** Elements the documentation renders; raw HTML and images stay out. */
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

type HeadingTag = "h1" | "h2" | "h3" | "h4";

/** Concatenates the text of a React subtree, which is a heading's plain text. */
function textOf(node: ReactNode): string {
  if (typeof node === "string") return node;
  if (typeof node === "number") return String(node);
  if (Array.isArray(node)) return node.map(textOf).join("");
  if (node !== null && typeof node === "object" && "props" in node) {
    return textOf((node.props as { readonly children?: ReactNode }).children);
  }
  return "";
}

/**
 * One heading that publishes a GitHub-style `id`, so a `#anchor` link and an
 * in-page link from another document can find it.
 */
function headingComponent(tag: HeadingTag) {
  return function HelpHeading({
    children,
    node: _node,
    ...rest
  }: ComponentPropsWithoutRef<"h2"> & ExtraProps): React.JSX.Element {
    const id = slugifyHeading(textOf(children));
    if (tag === "h1") return <h1 id={id || undefined} {...rest}>{children}</h1>;
    if (tag === "h3") return <h3 id={id || undefined} {...rest}>{children}</h3>;
    if (tag === "h4") return <h4 id={id || undefined} {...rest}>{children}</h4>;
    return <h2 id={id || undefined} {...rest}>{children}</h2>;
  };
}

interface DocumentationMarkdownProps {
  readonly markdown: string;
  /** Every activation is routed through the panel's own link handling. */
  readonly onFollow: (target: UserHelpTarget) => void;
}

/**
 * React Markdown's default transform blanks every URL scheme it does not know,
 * and Aworkit's own `aworkit:documents` link is one of them; without this the
 * link would render with an empty href and act on nothing. The panel's link is
 * preserved exactly and every other URL keeps the default safety check.
 */
function urlTransform(value: string): string {
  return value.trim().toLowerCase() === DOCUMENTS_FOLDER_HREF
    ? DOCUMENTS_FOLDER_HREF
    : defaultUrlTransform(value);
}

/**
 * Renders one help document as CommonMark plus GFM tables.
 *
 * The panel owns navigation, so every link activation is intercepted and handed
 * to `onFollow`; the rendered `href` only keeps the link a real, focusable link.
 * React Markdown's URL transform still rejects unsafe protocols before render.
 */
export function DocumentationMarkdown({
  markdown,
  onFollow,
}: DocumentationMarkdownProps): React.JSX.Element {
  const components = useMemo(() => {
    const link = ({
      href,
      children,
      node: _node,
      ...rest
    }: ComponentPropsWithoutRef<"a"> & ExtraProps): React.JSX.Element => {
      const target = resolveUserHelpTarget(href);
      return (
        <a
          {...rest}
          href={href === undefined ? "#" : href}
          rel={target.kind === "external" ? "noreferrer" : rest.rel}
          target={target.kind === "external" ? "_blank" : rest.target}
          onClick={(event) => {
            event.preventDefault();
            onFollow(target);
          }}
        >
          {children}
        </a>
      );
    };
    return {
      a: link,
      h1: headingComponent("h1"),
      h2: headingComponent("h2"),
      h3: headingComponent("h3"),
      h4: headingComponent("h4"),
    };
  }, [onFollow]);
  return (
    <ReactMarkdown
      allowedElements={[...renderedElements]}
      components={components}
      remarkPlugins={[remarkGfm]}
      unwrapDisallowed
      urlTransform={urlTransform}
    >
      {markdown}
    </ReactMarkdown>
  );
}
