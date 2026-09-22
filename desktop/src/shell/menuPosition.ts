/** Viewport-fixed position of an open context menu. */
export interface MenuPosition {
  readonly left: number;
  readonly top: number;
}

/**
 * Keeps a viewport-fixed menu visible regardless of the scroll offset of the
 * surface that opened it, so a menu near an edge stays fully reachable.
 */
export function fitMenuToViewport(
  position: MenuPosition,
  width: number,
  height: number,
): MenuPosition {
  const padding = 8;
  const maximumLeft = Math.max(padding, window.innerWidth - width - padding);
  const maximumTop = Math.max(padding, window.innerHeight - height - padding);
  return {
    left: Math.min(Math.max(position.left, padding), maximumLeft),
    top: Math.min(Math.max(position.top, padding), maximumTop),
  };
}
