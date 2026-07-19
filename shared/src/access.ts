export type ConversationAccessMode = "full" | "ask";

export function providerLabel(provider: import("./types").AgentProvider): string {
  return provider === "kimi" ? "Kimi" : "Codex";
}

/** Options accepted by Codex app-server thread/start. */
export function threadOptionsForAccess(mode: ConversationAccessMode): Record<string, unknown> {
  return mode === "full"
    ? { approvalPolicy: "never", sandbox: "danger-full-access" }
    : { approvalPolicy: "on-request", sandbox: "workspace-write" };
}

/** Options accepted by Codex app-server turn/start. */
export function turnOptionsForAccess(mode: ConversationAccessMode): Record<string, unknown> {
  return mode === "full"
    ? { approvalPolicy: "never", sandboxPolicy: { type: "dangerFullAccess" } }
    : {
        approvalPolicy: "on-request",
        sandboxPolicy: { type: "workspaceWrite", networkAccess: false },
      };
}
