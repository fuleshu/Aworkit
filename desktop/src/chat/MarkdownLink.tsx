import { isTauri } from "@tauri-apps/api/core";
import { openUrl } from "@tauri-apps/plugin-opener";
import type { ComponentPropsWithoutRef, MouseEvent } from "react";
import type { ExtraProps } from "react-markdown";
import { useNotificationPublisher } from "../notifications/NotificationContext";

/** Opens model web citations through the OS without navigating the app WebView. */
export function MarkdownLink({
  node: _node,
  href,
  ...properties
}: ComponentPropsWithoutRef<"a"> & ExtraProps): React.JSX.Element {
  const notifications = useNotificationPublisher("Chat", "web-links", "chat");
  // Relative paths and other schemes are not external web citations. Keep their
  // label visible without allowing model output to navigate the desktop shell.
  const webUrl = /^https?:\/\//i.test(href ?? "") ? href : undefined;
  const activate = (event: MouseEvent<HTMLAnchorElement>) => {
    event.stopPropagation();
    if (webUrl === undefined) {
      event.preventDefault();
      return;
    }
    // A browser-hosted preview keeps standard new-tab and modifier-key behavior.
    if (!isTauri()) return;
    event.preventDefault();
    void openUrl(webUrl).catch(() => {
      notifications.publish("open-link", {
        summary: "Could not open the link in your default browser.",
        detail: webUrl,
        severity: "error",
        lifetime: { kind: "transient" },
      });
    });
  };
  return (
    <a
      {...properties}
      href={webUrl}
      rel="noreferrer"
      target="_blank"
      onClick={activate}
      onAuxClick={(event) => {
        if (event.button === 1) activate(event);
      }}
    />
  );
}
