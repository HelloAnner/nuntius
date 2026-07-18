/* Overview: this machine at a glance — agent, app server, pairing, queues. */
import { useQuery } from "@tanstack/react-query";
import {
  Avatar,
  IconCheck,
  IconX,
  Pill,
  Spinner,
  initials,
  tintIndex,
} from "@nuntius/shared";
import { api } from "../api";
import { ConnIndicator, TopBar } from "../components";

export function OverviewPage() {
  const info = useQuery({
    queryKey: ["info"],
    queryFn: api.info,
    refetchInterval: 10_000,
    retry: false,
  });
  const down = info.isError;
  const data = info.data;

  return (
    <div className="page">
      <TopBar title={<span className="wordmark">本机控制台</span>} trailing={<ConnIndicator />} />
      <div className="page-scroll">
        <div className="page-col" style={{ maxWidth: 640 }}>
          {info.isLoading ? (
            <div style={{ display: "grid", placeItems: "center", padding: 60 }}>
              <Spinner />
            </div>
          ) : down ? (
            <>
              <div className="hero">
                <Avatar text="?" tint={1} />
                <div className="meta">
                  <div className="name display">服务未运行</div>
                  <div className="facts">无法连接本地 Nuntius 服务</div>
                </div>
                <Pill tone="danger">离线</Pill>
              </div>
              <div className="notice-banner warn">
                本地服务没有响应。在终端运行 <span className="mono">nuntius-client start</span>{" "}
                启动后台服务，或使用 <span className="mono">nuntius-client run</span> 前台调试。
              </div>
              <button className="btn ghost block" onClick={() => void info.refetch()}>
                重新检测
              </button>
            </>
          ) : data ? (
            <>
              <div className="hero">
                <Avatar text={initials(hostnameOf(data.deviceId))} tint={tintIndex(data.deviceId)} online />
                <div className="meta">
                  <div className="name display">{hostnameOf(data.deviceId)}</div>
                  <div className="facts">
                    <span className="mono" style={{ overflow: "hidden", textOverflow: "ellipsis" }}>
                      {data.deviceId.length > 20 ? `${data.deviceId.slice(0, 12)}…${data.deviceId.slice(-6)}` : data.deviceId}
                    </span>
                    <span>·</span>
                    <span>CLI {data.clientVersion}</span>
                    <span>·</span>
                    <span className="mono">{data.localBind}</span>
                  </div>
                </div>
                <Pill tone="ok" pulse>
                  运行中
                </Pill>
              </div>

              <div className="section-label micro">运行状态</div>
              <div className="list-group">
                <StatusRow
                  ok
                  title="本地服务"
                  detail="数据库与本地 API 正常"
                />
                <StatusRow
                  ok={data.appServerRunning}
                  title="Codex App Server"
                  detail={
                    data.appServerRunning
                      ? "进程运行中，可以发起对话"
                      : "未运行。确认已安装 Codex，并用 nuntius-client run 查看原因"
                  }
                />
                <StatusRow
                  ok={data.paired}
                  title="公网连接"
                  detail={
                    data.paired
                      ? "已配对，事件正在同步到服务器"
                      : "未配对。在服务器「设置」页生成配对码后运行 nuntius-client pair <CODE>"
                  }
                />
                <StatusRow
                  ok={data.pendingCommands === 0 && data.pendingEvents === 0}
                  title="同步队列"
                  detail={
                    data.pendingCommands === 0 && data.pendingEvents === 0
                      ? "没有积压的命令或事件"
                      : `待处理命令 ${data.pendingCommands} · 待同步事件 ${data.pendingEvents}`
                  }
                />
              </div>

              <div className="section-label micro">本机概况</div>
              <div className="fact-grid">
                <div className="fact">
                  <div className="k">项目</div>
                  <div className="v num">{data.projects}</div>
                </div>
                <div className="fact">
                  <div className="k">活跃 Turn</div>
                  <div className="v num">{data.activeTurns}</div>
                </div>
                <div className="fact">
                  <div className="k">待处理命令</div>
                  <div className="v num">{data.pendingCommands}</div>
                </div>
                <div className="fact">
                  <div className="k">待同步事件</div>
                  <div className="v num">{data.pendingEvents}</div>
                </div>
              </div>

              <p style={{ margin: "26px 0 10px", textAlign: "center", fontSize: 12, color: "var(--ink-4)" }}>
                此页面只连接本机 loopback 服务，公网不可用时依然可以管理本机项目与会话
              </p>
            </>
          ) : null}
        </div>
      </div>
    </div>
  );
}

function StatusRow({ ok, title, detail }: { ok: boolean; title: string; detail: string }) {
  return (
    <div className="list-row">
      <span className={`row-glyph${ok ? "" : " muted"}`} style={ok ? {} : { background: "var(--warn-soft)", color: "var(--warn)" }}>
        {ok ? <IconCheck size={16} /> : <IconX size={16} />}
      </span>
      <div className="grow">
        <div className="title">{title}</div>
        <div className="sub">
          <span className="ellipsis">{detail}</span>
        </div>
      </div>
      <Pill tone={ok ? "ok" : "warn"}>{ok ? "正常" : "待处理"}</Pill>
    </div>
  );
}

function hostnameOf(deviceId: string): string {
  if (deviceId === "unpaired") return "这台电脑";
  return deviceId.replace(/^dev_/, "").slice(0, 8);
}
