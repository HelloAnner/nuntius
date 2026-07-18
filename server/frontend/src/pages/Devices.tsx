/* Devices home: every machine at a glance, online first. */
import { useQuery } from "@tanstack/react-query";
import { Empty, IconDevice, Spinner } from "@nuntius/shared";
import { api } from "../api";
import { ConnIndicator, DeviceRow, TopBar } from "../components";

export function DevicesPage() {
  const devices = useQuery({ queryKey: ["devices"], queryFn: api.devices });
  const sorted = [...(devices.data ?? [])].sort((a, b) => {
    const rank = (s: string) => (s === "online" ? 0 : s === "syncing" || s === "degraded" ? 1 : 2);
    return rank(a.status) - rank(b.status);
  });

  return (
    <div className="page">
      <TopBar title={<span className="wordmark">Nuntius</span>} trailing={<ConnIndicator />} />
      <div className="page-scroll">
        <div className="page-col">
          {devices.isLoading ? (
            <DeviceSkeletons />
          ) : sorted.length === 0 ? (
            <Empty
              icon={<IconDevice size={24} />}
              headline="还没有设备"
              hint="在电脑上用配对码运行 nuntius-client pair，即可把这台电脑接入控制。配对码可在「设置」中生成。"
            />
          ) : (
            <>
              <div className="section-label micro">我的设备 · {sorted.length}</div>
              <div className="list-group">
                {sorted.map((d) => (
                  <DeviceRow key={d.id} device={d} />
                ))}
              </div>
              <p style={{ marginTop: 18, fontSize: 12, color: "var(--ink-4)", textAlign: "center" }}>
                设备离线时仍可阅读已同步的会话历史
              </p>
            </>
          )}
        </div>
      </div>
    </div>
  );
}

function DeviceSkeletons() {
  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 10, marginTop: 8 }}>
      {[0, 1].map((i) => (
        <div key={i} className="list-row" style={{ border: "1px solid var(--hairline)", borderRadius: "var(--r-lg)" }}>
          <div className="skeleton" style={{ width: 40, height: 40, borderRadius: 13 }} />
          <div className="grow" style={{ display: "flex", flexDirection: "column", gap: 7 }}>
            <div className="skeleton" style={{ height: 14, width: "45%" }} />
            <div className="skeleton" style={{ height: 11, width: "70%" }} />
          </div>
          <Spinner sm />
        </div>
      ))}
    </div>
  );
}
