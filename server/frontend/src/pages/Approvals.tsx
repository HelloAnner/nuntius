/* Approvals inbox: pending first, decided below. */
import { useMemo } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  ApprovalCard,
  Empty,
  IconShield,
  newIdemKey,
  relTime,
  useToast,
  type ApprovalView,
} from "@nuntius/shared";
import { api, ApiError } from "../api";
import { trackCommand } from "../events";
import { useNavigate } from "../hooks";
import { useApprovals } from "../stores";
import { ConnIndicator, TopBar } from "../components";

export function ApprovalsPage() {
  const toast = useToast();
  const qc = useQueryClient();
  const navigate = useNavigate();
  const { items, order } = useApprovals();
  const devices = useQuery({ queryKey: ["devices"], queryFn: api.devices });
  const threads = useQuery({ queryKey: ["allThreads"], queryFn: () => api.allThreads() });

  const enrich = (a: ApprovalView): ApprovalView => ({
    ...a,
    deviceName: devices.data?.find((d) => d.id === a.deviceId)?.displayName,
    threadTitle: threads.data?.find((t) => t.id === a.threadId)?.title,
  });

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
      const receipt = await api.decideApproval(a.deviceId, a.id, decision, newIdemKey());
      trackCommand(qc, receipt.commandId, a.threadId ?? undefined, "approval.decide");
      store.setState(a.id, decision === "decline" || decision === "cancel" ? "denied" : "approved", decision);
    } catch (e) {
      store.setState(a.id, "pending");
      toast(
        e instanceof ApiError && e.code === "device_offline" ? "设备离线，决定未送达" : "提交失败，请重试",
        { error: true },
      );
    }
  };

  const approvalConnected = (approval: ApprovalView) => {
    const device = devices.data?.find((candidate) => candidate.id === approval.deviceId);
    if (device?.status !== "online") return false;
    const provider = threads.data?.find((thread) => thread.id === approval.threadId)?.provider ?? "codex";
    const providers = device.providers ?? [];
    const providerStatus = providers.find((status) => status.provider === provider);
    return providerStatus?.status === "online" || (provider === "codex" && providers.length === 0);
  };

  return (
    <div className="page approvals-page">
      <TopBar title="审批" subtitle="集中处理所有设备和会话发起的权限请求" trailing={<ConnIndicator />} />
      <div className="page-scroll">
        <div className="page-col console-page-col narrow-page-col">
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
                  {pending.map((a) => {
                    const enriched = enrich(a);
                    return (
                      <div key={a.id} className="approval-inbox-item">
                        <ApprovalCard
                          approval={enriched}
                          onDecide={(d) => void decide(a, d)}
                          locked={!approvalConnected(a)}
                        />
                        {a.threadId ? (
                          <button
                            className="btn quiet sm approval-thread-link"
                            onClick={() => {
                              const t = threads.data?.find((x) => x.id === a.threadId);
                              if (t) {
                                navigate({
                                  name: "thread",
                                  deviceId: t.deviceId,
                                  projectId: t.projectId,
                                  threadId: t.id,
                                });
                              }
                            }}
                          >
                            查看相关会话 →
                          </button>
                        ) : null}
                      </div>
                    );
                  })}
                </>
              ) : null}
              {decided.length > 0 ? (
                <>
                  <div className="section-label micro">已处理</div>
                  <div className="list-group">
                    {decided.map((a) => {
                      const e = enrich(a);
                      return (
                        <div key={a.id} className="list-row decided-approval-row">
                          <div className="grow">
                            <div className="title">
                              {e.threadTitle ?? "审批"} · {labelOf(e)}
                            </div>
                            <div className="sub">
                              <span>{e.deviceName}</span>
                              <span>·</span>
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
