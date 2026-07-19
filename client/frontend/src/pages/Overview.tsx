/* Overview: this machine at a glance — providers, pairing, and queues. */
import { useQuery } from "@tanstack/react-query";
import {
  Avatar,
  Empty,
  IconDevice,
  IconX,
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
  const queueBusy = Boolean(data && (data.pendingCommands > 0 || data.pendingEvents > 0));
  const hasProvider = Boolean(data?.providers.some((provider) => provider.available));
  const issueCount = data
    ? Number(!hasProvider) + Number(!data.paired) + Number(queueBusy)
    : 0;

  return (
    <div className="page">
      <TopBar title={<span className="wordmark">本机</span>} trailing={<ConnIndicator />} />
      <div className="page-scroll">
        <div className="page-col" style={{ maxWidth: 640 }}>
          {info.isLoading ? (
            <div style={{ display: "grid", placeItems: "center", padding: 60 }}>
              <Spinner />
            </div>
          ) : down ? (
            <Empty
              icon={<IconDevice size={24} />}
              headline="本地服务未运行"
              hint="启动 nuntius-client 后重新检测"
              action={
                <button className="btn ghost" onClick={() => void info.refetch()}>
                  重新检测
                </button>
              }
            />
          ) : data ? (
            <>
              <div className="hero">
                <Avatar text={initials(data.displayName)} tint={tintIndex(data.deviceId)} online />
                <div className="meta">
                  <div className="name display">{data.displayName}</div>
                  <div className="facts">
                    <span>CLI {data.clientVersion}</span>
                    <span className="mono">{data.localBind}</span>
                  </div>
                </div>
              </div>

              {issueCount > 0 ? (
                <>
                  <div className="section-label micro">需要处理 · {issueCount}</div>
                  <div className="list-group">
                    {!hasProvider ? (
                      <IssueRow title="没有可用的编码代理" detail="请安装 Codex 或 Kimi CLI" />
                    ) : null}
                    {!data.paired ? (
                      <IssueRow title="尚未配对" detail="可在远程控制台的设置页获取配对码" />
                    ) : null}
                    {queueBusy ? (
                      <IssueRow
                        title="等待同步"
                        detail={`${data.pendingCommands} 个命令 · ${data.pendingEvents} 个事件`}
                      />
                    ) : null}
                  </div>
                </>
              ) : null}

              <div className="section-label micro">概况</div>
              <div className="fact-grid">
                <div className="fact">
                  <div className="k">项目</div>
                  <div className="v num">{data.projects}</div>
                </div>
                <div className="fact">
                  <div className="k">运行中</div>
                  <div className="v num">{data.activeTurns}</div>
                </div>
              </div>
            </>
          ) : null}
        </div>
      </div>
    </div>
  );
}

function IssueRow({ title, detail }: { title: string; detail?: string }) {
  return (
    <div className="list-row">
      <span className="row-glyph muted" style={{ background: "var(--warn-soft)", color: "var(--warn)" }}>
        <IconX size={16} />
      </span>
      <div className="grow">
        <div className="title">{title}</div>
        {detail ? <div className="sub"><span className="ellipsis">{detail}</span></div> : null}
      </div>
    </div>
  );
}
