import { createDurableCommandId } from "../commandId";
import type { ChatIntent } from "./types";

/** Creates typed Chat intents; event ownership stays in useChatRuntime. */
export class ChatWorkspaceController {
  public createIntent(type: ChatIntent["type"], targetId?: string): ChatIntent {
    const commandId = createDurableCommandId("chat");
    if (type === "start")
      return {
        type,
        commandId,
        workflowId: "",
        projectId: null,
        input: "",
        attachments: [],
      };
    if (type === "enqueue") return { type, commandId, input: "" };
    if (type === "approval")
      return { type, commandId, decisionId: targetId ?? "", approved: false };
    return { type, commandId, targetId };
  }
}
