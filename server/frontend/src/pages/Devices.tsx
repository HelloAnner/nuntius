/* Device fleet overview and connection diagnostics. */
import { useQuery } from "@tanstack/react-query";
import { Empty, IconDevice, IconPlus } from "@nuntius/shared";
import { api } from "../api";
import { useNavigate } from "../hooks";
import { DeviceRow, StatusDot, TopBar } from "../components";

export function DevicesPage() {
  const navigate = useNavigate();
  const devices = useQuery({ queryKey: ["devices"], queryFn: api.devices });
  const sorted = [...(devices.data ?? [])].sort((a, b) => {
    const rank = (status: string) => (status === "online" ? 0 : status === "syncing" || status === "degraded" ? 1 : 2);
    return rank(a.status) - rank(b.status);
  });
  const onlineCount = sorted.filter((device) => device.status === "online").length;

  return (
    <div className="page devices-page">
      <TopBar
        title="设备"
        subtitle={`${sorted.length} 台已配对设备 · ${onlineCount} 台在线 · 离线设备的历史仍可阅读`}
        trailing={
          <div className="page-actions">
            <button className="btn outline desktop-only" onClick={() => navigate({ name: "settings" })}>
              配对指南
            </button>
            <button className="btn primary" onClick={() => navigate({ name: "settings" })} aria-label="配对新设备">
              <IconPlus size={16} />
              <span className="desktop-only">配对新设备</span>
            </button>
          </div>
        }
      />
      <div className="page-scroll">
        <div className="page-col console-page-col">
          {devices.isLoading ? (
            <DeviceSkeletons />
          ) : sorted.length === 0 ? (
            <Empty
              icon={<IconDevice size={24} />}
              headline="还没有设备"
              hint="生成配对码，在要接入的电脑上完成配对"
              action={<button className="btn primary" onClick={() => navigate({ name: "settings" })}>开始配对</button>}
            />
          ) : (
            <>
              <div className="device-grid">
                {sorted.map((device) => <DeviceRow key={device.id} device={device} />)}
              </div>
              <section className="diagnostics-panel">
                <header>
                  <strong>连接与同步诊断</strong>
                  <span>状态随实时事件更新</span>
                </header>
                <div className="diagnostics-table-wrap">
                  <table>
                    <thead>
                      <tr>
                        <th>设备</th>
                        <th>设备通道</th>
                        <th>Agent 提供方</th>
                        <th>历史同步</th>
                        <th>待审批</th>
                      </tr>
                    </thead>
                    <tbody>
                      {sorted.map((device) => {
                        const channelTone = device.status === "online" ? "success" : device.status === "syncing" ? "warning" : "offline";
                        const providers = device.providers.filter((provider) => provider.available);
                        return (
                          <tr key={device.id}>
                            <td><strong>{device.displayName}</strong></td>
                            <td><DiagnosticValue tone={channelTone} text={device.status === "online" ? "已连接" : device.status === "syncing" ? "同步中" : "未连接"} /></td>
                            <td>
                              <DiagnosticValue
                                tone={providers.length ? "success" : device.status === "online" ? "warning" : "offline"}
                                text={providers.length ? providers.map((provider) => provider.label).join(" · ") : "不可用"}
                              />
                            </td>
                            <td>
                              <DiagnosticValue
                                tone={device.historyCompleteness === "complete" ? "success" : device.historyCompleteness === "error" ? "danger" : "warning"}
                                text={historyLabel(device.historyCompleteness)}
                              />
                            </td>
                            <td className="num">{device.pendingApprovalCount || "—"}</td>
                          </tr>
                        );
                      })}
                    </tbody>
                  </table>
                </div>
              </section>
              <button className="mobile-pair-card" onClick={() => navigate({ name: "settings" })}>
                <span className="device-icon"><IconPlus size={18} /></span>
                <span><strong>配对新设备</strong><small>生成配对码并运行 nuntius-client</small></span>
              </button>
            </>
          )}
        </div>
      </div>
    </div>
  );
}

function DiagnosticValue({ tone, text }: { tone: "success" | "warning" | "danger" | "offline"; text: string }) {
  return <span className="diagnostic-value"><StatusDot tone={tone} />{text}</span>;
}

function historyLabel(status: string) {
  switch (status) {
    case "complete": return "已同步";
    case "backfilling": return "同步中";
    case "partial": return "部分同步";
    case "error": return "同步异常";
    default: return "尚未同步";
  }
}

function DeviceSkeletons() {
  return (
    <div className="device-grid" aria-label="正在加载设备">
      {[0, 1, 2].map((index) => (
        <div key={index} className="device-card skeleton-card">
          <div className="skeleton skeleton-title" />
          <div className="device-stats">
            <span className="skeleton" /><span className="skeleton" /><span className="skeleton" />
          </div>
          <div className="skeleton skeleton-line" />
        </div>
      ))}
    </div>
  );
}
