// @vitest-environment jsdom
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it } from "vitest";
import { ChatRecoveryCard } from "./ChatRecoveryCard";
import { useChatRuntime } from "./useChatRuntime";
import { chatSnapshot } from "../test/fixtures/chat";
import type { ChatCorePort } from "./corePort";
import type { ChatIntent } from "./types";

afterEach(cleanup);

function Harness({ port }: { port: ChatCorePort }): React.JSX.Element {
  const runtime = useChatRuntime(port, 60_000);
  const chat = runtime.snapshot?.chat;
  return <>
    <button onClick={() => void runtime.dispatch({ type: "select_chat", commandId: crypto.randomUUID(), targetId: chat?.chatId === "chat.a" ? "chat.b" : "chat.a" })}>Switch Chat</button>
    <button onClick={() => void runtime.resynchronize()}>Refresh</button>
    {chat && <ChatRecoveryCard key={chat.chatId} chatId={chat.chatId} recoveryPending={chat.recoveryPending} runtime={runtime} nextCommandId={() => crypto.randomUUID()} />}
  </>;
}

it("keeps recovery progress visible until the command settles and blocks duplicate clicks", async () => {
  const user = userEvent.setup();
  let release!: () => void;
  const waiting = new Promise<void>(resolve => { release = resolve; });
  let recoveryPending = true;
  const calls: ChatIntent[] = [];
  const port: ChatCorePort = {
    async snapshot() { return chatSnapshot({ throughSequence: 0, chat: { chatId: "chat.a", recoveryPending } }); },
    async command(intent) {
      calls.push(intent);
      recoveryPending = false; // Native snapshot hides crash recovery while a worker is active.
      await waiting;
      return { commandId: intent.commandId, accepted: true, currentVersion: 0, reason: null };
    },
  };
  render(<Harness port={port} />);
  await user.dblClick(await screen.findByRole("button", { name: "Continue reply" }));
  await user.click(screen.getByText("Refresh"));
  expect(screen.getByText("Continuing your reply…")).toBeVisible();
  expect(screen.getByRole("button", { name: "Continuing…" })).toBeDisabled();
  expect(calls).toHaveLength(1);
  await act(async () => release());
  await waitFor(() => expect(screen.queryByRole("region", { name: "Interrupted reply" })).toBeNull());
});

it("keeps stop confirmation and late errors with their owning Chat", async () => {
  const user = userEvent.setup();
  let selected = "chat.a";
  let release!: () => void;
  const waiting = new Promise<void>(resolve => { release = resolve; });
  const calls: ChatIntent[] = [];
  const port: ChatCorePort = {
    async snapshot(_after, chatId = selected) { return chatSnapshot({ throughSequence: 0, chat: { chatId, recoveryPending: chatId === "chat.a" } }); },
    async command(intent) {
      if (intent.type === "select_chat") selected = intent.targetId;
      else { calls.push(intent); await waiting; }
      return { commandId: intent.commandId, accepted: intent.type === "select_chat", currentVersion: 0, reason: "Fixture recovery failed" };
    },
  };
  render(<Harness port={port} />);
  await user.click(await screen.findByRole("button", { name: "Stop reply" }));
  await user.click(screen.getByText("Switch Chat"));
  await waitFor(() => expect(screen.queryByText("Stop this reply?")).toBeNull());
  await user.click(screen.getByText("Switch Chat"));
  await user.click(await screen.findByRole("button", { name: "Continue reply" }));
  await user.click(screen.getByText("Switch Chat"));
  await act(async () => release());
  expect(screen.queryByText("Show details")).toBeNull();
  await user.click(screen.getByText("Switch Chat"));
  await user.click(await screen.findByText("Show details"));
  expect(screen.getByText("Fixture recovery failed")).toBeVisible();
  expect(calls).toHaveLength(1);
  expect(calls[0]).toMatchObject({ type: "resume", targetId: "chat.a" });
});
