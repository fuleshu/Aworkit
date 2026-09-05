import { useEffect, useState, type ReactElement, type ReactNode } from "react";
import { render as renderComponent, type RenderOptions } from "@testing-library/react";
import { NotificationStore } from "../notifications/NotificationStore";
import { NotificationProvider } from "../notifications/NotificationContext";
import { DesktopStatusBar } from "../notifications/DesktopStatusBar";

/** Feature integration tests use the real window notification boundary. */
function TestNotificationWindow({ children }: { readonly children: ReactNode }): React.JSX.Element {
  const [store] = useState(() => new NotificationStore());
  useEffect(() => () => store.dispose(), [store]);
  return <NotificationProvider store={store}>{children}<DesktopStatusBar store={store} route="" /></NotificationProvider>;
}

export function render(ui: ReactElement, options?: Omit<RenderOptions, "wrapper">) {
  return renderComponent(ui, { ...options, wrapper: TestNotificationWindow });
}
