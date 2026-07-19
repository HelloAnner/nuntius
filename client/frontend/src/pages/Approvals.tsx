/* Local approvals inbox. */
import { useMemo } from "react";
import { useQuery } from "@tanstack/react-query";
import {
  ApprovalCard,
  Empty,
  IconShield,
  relTime,
  useToast,
  type ApprovalView,
} from "@nuntius/shared";
import { api } from "../api";
import { useNavigate } from "../hooks";
import { useApprovals } from "../stores";
import { ConnIndicator, TopBar } from "../components";

export function ApprovalsPage() {
  const toast = useToast();
  const navigate = useNavigate();
  const { items, order } = useApprovals();
  const threads = useQuery({ queryKey: ["threads"], queryFn: api.threads });
  const info = useQuery({ queryKey: ["info"], queryFn: api.info });

  const enrich = (a: ApprovalView): ApprovalView => ({
    ...a,
    threadTitle: threads.data?.find((t) => t.id === a.threadId)?.title,
  });
  const approvalProviderConnected = (approval: ApprovalView) => {
    const provider = threads.data?.find((thread) => thread.id === approval.threadId)?.provider ?? "codex";
    return info.data?.providers.find((status) => status.provider === provider)?.status === "online";
  };

  const { pending, decided } = useMemo(() => {
    const all = order
      .map((id) => items[id])
      .filter(Boolean)
      .sort((a, b) => Date.parse(b.occurredAt) - Date.parse(a.occurredAt));
    return {
      pending: all.filter((a) => a.state === "pending" || a.state === "responding"),
      decided: all.filter((a) => a.state !== "pending" && a.state !== "responding").slice(0, 20),
    };
  }, [items, order]);

  const decide = async (a: ApprovalView, decision: string) => {
    const store = useApprovals.getState();
    store.setState(a.id, "responding");
    try {
      await api.decideApproval(a.id, decision);
      store.setState(a.id, decision === "decline" || decision === "cancel" ? "denied" : "approved", decision);
    } catch (e) {
      store.setState(a.id, "pending");
      toast(e instanceof Error ? e.message : "提交失败，请重试", { error: true });
    }
  };

  return (
    <div className="page">
      <TopBar title="审批" trailing={<ConnIndicator />} />
      <div className="page-scroll">
        <div className="page-col" style={{ maxWidth: 640 }}>
          {pending.length === 0 && decided.length === 0 ? (
            <Empty
              icon={<IconShield size={24} />}
              headline="没有待处理的审批"
            />
          ) : (
            <>
              {pending.length > 0 ? (
                <>
                  <div className="section-label micro">待处理 · {pending.length}</div>
                  {pending.map((a) => (
                    <div key={a.id} style={{ marginBottom: 14 }}>
                      <ApprovalCard
                        approval={enrich(a)}
                        onDecide={(d) => void decide(a, d)}
                        locked={!approvalProviderConnected(a)}
                      />
                      {a.threadId ? (
                        <button
                          className="btn quiet sm"
                          style={{ marginTop: 6 }}
                          onClick={() => {
                            const t = threads.data?.find((x) => x.id === a.threadId);
                            if (t) {
                              navigate({ name: "thread", projectId: t.projectId, threadId: t.id });
                            }
                          }}
                        >
                          查看相关会话 →
                        </button>
                      ) : null}
                    </div>
                  ))}
                </>
              ) : null}
              {decided.length > 0 ? (
                <>
                  <div className="section-label micro">已处理</div>
                  <div className="list-group">
                    {decided.map((a) => {
                      const e = enrich(a);
                      return (
                        <div key={a.id} className="list-row" style={{ minHeight: 52 }}>
                          <div className="grow">
                            <div className="title" style={{ fontSize: 14 }}>
                              {e.threadTitle ?? "审批"} · {labelOf(e)}
                            </div>
                            <div className="sub">
                              <span className="num">{relTime(e.occurredAt)}</span>
                            </div>
                          </div>
                        </div>
                      );
                    })}
                  </div>
                </>
              ) : null}
            </>
          )}
        </div>
      </div>
    </div>
  );
}

function approvalProviderOnline(
  approval: ApprovalView,
  threads: import("@nuntius/shared").ThreadSummary[] | undefined,
  providers: import("@nuntius/shared").AgentProviderStatus[] | undefined,
): boolean {
  const provider = threads?.find((thread) => thread.id === approval.threadId)?.provider ?? "codex";
  return providers?.find((status) => status.provider === provider)?.status === "online";
}

function labelOf(a: ApprovalView): string {
  switch (a.decidedAs) {
    case "accept":
      return "已批准";
    case "accept_for_session":
      return "本会话内批准";
    case "decline":
      return "已拒绝";
    case "cancel":
      return "已取消";
    default:
      return a.state === "cancelled" ? "已取消" : "已结束";
  }
}
