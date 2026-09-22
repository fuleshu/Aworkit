import { isTauri } from "@tauri-apps/api/core";
import {
  cloneElement,
  useState,
  type MouseEvent,
  type ReactElement,
} from "react";
import { PathContextMenu } from "./PathContextMenu";
import { usePathAction } from "./pathActions";

interface PathMenuSpots {
  readonly onContextMenu?: (event: MouseEvent<HTMLElement>) => void;
}

/**
 * Makes one element that already shows a path actionable by right-click.
 *
 * The element keeps its own left-click behavior, which for a conversation path
 * is deliberately inert; only the explicit menu asks the core to act. Without a
 * path, without a Chat to resolve it against, and outside the desktop
 * application the element is returned untouched, so a browser preview never
 * offers an action it cannot perform.
 */
export function PathMenuTarget({
  path,
  children,
}: {
  /** The path exactly as the conversation or tool call showed it. */
  readonly path: string | undefined;
  readonly children: ReactElement<PathMenuSpots>;
}): React.JSX.Element {
  const run = usePathAction();
  const [menu, setMenu] = useState<{ left: number; top: number } | null>(null);
  if (path === undefined || path === "" || run === null || !isTauri())
    return children;
  const inherited = children.props.onContextMenu;
  return (
    <>
      {cloneElement(children, {
        onContextMenu: (event: MouseEvent<HTMLElement>) => {
          inherited?.(event);
          if (event.defaultPrevented) return;
          event.preventDefault();
          setMenu({ left: event.clientX, top: event.clientY });
        },
      })}
      {menu !== null && (
        <PathContextMenu
          key={`${path}:${menu.left}:${menu.top}`}
          onClose={() => setMenu(null)}
          path={path}
          position={menu}
        />
      )}
    </>
  );
}
