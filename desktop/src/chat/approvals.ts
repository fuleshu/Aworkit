import { z } from "zod";

export const approvalModeSchema = z.enum(["ask_for_approval", "approve_for_me", "full_access"]);
export type ApprovalMode = z.infer<typeof approvalModeSchema>;
export type ApprovalChoice = "approve_once" | "approve_for_chat" | "always_approve_in_project" | "deny";
export const filesystemRequestSchema = z.object({ access: z.enum(["read", "write"]), directory: z.string() }).strict();
export type FilesystemRequest = z.infer<typeof filesystemRequestSchema>;
export interface FilesystemSelection {
  readonly access: "read" | "write";
  readonly location: "directory" | "all_external";
  readonly directory?: string;
}
export interface ApprovalActionDetails { readonly choice: ApprovalChoice; readonly reason?: string; readonly filesystem?: FilesystemSelection; }

export const approvalModes: readonly { value: ApprovalMode; label: string; description: string }[] = [
  { value: "ask_for_approval", label: "Ask for approval", description: "Ask you before running an action that needs approval." },
  { value: "approve_for_me", label: "Approve for me", description: "An independent model request reviews actions in context. Unclear actions come to you." },
  { value: "full_access", label: "Full access", description: "Run enabled tools without approval prompts or automatic review." },
];
