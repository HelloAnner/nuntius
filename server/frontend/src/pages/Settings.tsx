/* Settings: account, pairing codes, device revocation, theme, about. */
import { useState, type CSSProperties } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  Avatar,
  IconBolt,
  IconChevronRight,
  IconClock,
  IconDevice,
  IconKey,
  IconRefresh,
  IconShield,
  Segmented,
  Spinner,
  fullTime,
  initials,
  osLabel,
  relTime,
  statusLabel,
  tintIndex,
  useConfirmAction,
  useToast,
  type PairingCodeView,
  type ConversationAccessMode,
  type Theme,
  type DeviceSummary,
  type ProviderQuotaWindow,
  type ProviderUsageLatestView,
} from "@nuntius/shared";
import { api } from "../api";
import { useAccessMode, useSession, useThemeStore } from "../stores";
import { useNavigate } from "../hooks";
import { ConnIndicator, TopBar } from "../components";
import { RenameDeviceSheet } from "../sheets/RenameDeviceSheet";
import { fleetVersionState } from "../versioning";

export function SettingsPage() {
  const navigate = useNavigate();
  const toast = useToast();
  const qc = useQueryClient();
  const { session, setSession } = useSession();
  const { theme, setTheme } = useThemeStore();
  const { mode: accessMode, setMode: setAccessMode } = useAccessMode();
  const { confirm, node: confirmNode } = useConfirmAction();
  const [pairing, setPairing] = useState<PairingCodeView | null>(null);
  const [busyPairing, setBusyPairing] = useState(false);
  const [renaming, setRenaming] = useState<DeviceSummary | null>(null);
  const [refreshingUsage, setRefreshingUsage] = useState(false);

  const info = useQuery({ queryKey: ["info"], queryFn: api.info, staleTime: 60_000 });
  const devices = useQuery({ queryKey: ["devices"], queryFn: api.devices });
  const providerUsage = useQuery({
    queryKey: ["providerUsage"],
    queryFn: api.providerUsage,
  });
  const usageGroups = groupUsageByDevice(providerUsage.data ?? []);
  const pairedDevices = devices.data ?? [];
  const activeDevices = pairedDevices.filter((device) => device.status !== "revoked");
  const onlineDevices = pairedDevices.filter((device) => device.status === "online").length;
  const versionState = fleetVersionState(info.data?.serverVersion, devices.data);
  const versionStateLabel = {
    compatible: "全部一致",
    mismatch: "版本不一致",
    unknown: "等待确认",
  }[versionState];

  const newPairingCode = async () => {
    setBusyPairing(true);
    try {
      setPairing(await api.createPairingCode());
    } catch {
      toast("生成配对码失败", { error: true });
    } finally {
      setBusyPairing(false);
    }
  };

  const logout = () =>
    confirm({
      title: "退出登录？",
      body: "当前浏览器会话将被撤销，设备与历史数据不受影响。",
      confirmLabel: "退出登录",
      action: async () => {
        try {
          await api.logout();
        } finally {
          setSession(null);
          window.location.reload();
        }
      },
    });

  const refreshUsage = async () => {
    setRefreshingUsage(true);
    try {
      const response = await api.refreshAllProviderUsage();
      toast(
        response.commands.length > 0
          ? `已通知 ${response.commands.length} 台设备刷新额度`
          : "没有可刷新的设备",
      );
      window.setTimeout(() => void providerUsage.refetch(), 2_000);
      window.setTimeout(() => void providerUsage.refetch(), 8_000);
    } catch {
      toast("额度刷新请求失败", { error: true });
    } finally {
      setRefreshingUsage(false);
    }
  };

  const revoke = (deviceId: string, name: string) =>
    confirm({
      title: `撤销「${name}」的访问？`,
      body: "这台电脑将立即断开连接，必须重新配对才能接入。已同步的历史记录保留。",
      confirmLabel: "撤销设备",
      danger: true,
      action: async () => {
        try {
          await api.revokeDevice(deviceId);
          toast("设备已撤销");
          await qc.invalidateQueries({ queryKey: ["devices"] });
        } catch {
          toast("撤销失败", { error: true });
        }
      },
    });

  return (
    <div className="page settings-page">
      <TopBar title="设置" subtitle="账号、设备配对、代理权限与外观" trailing={<ConnIndicator />} />
      <div className="page-scroll">
        <div className="page-col console-page-col narrow-page-col">
          <div className="card settings-user">
            <Avatar text={initials(session?.loginName ?? "?")} tint={1} />
            <div className="grow settings-user-copy">
              <div className="title settings-user-name">
                {session?.loginName}
              </div>
              <div className="sub num">会话有效期至 {fullTime(session?.expiresAt)}</div>
            </div>
            <button className="btn ghost sm" onClick={logout}>
              退出
            </button>
          </div>

          <div className="section-label micro">设备与连接</div>
          <button
            className="card settings-destination"
            onClick={() => navigate({ name: "devices" })}
          >
            <span className="settings-destination-icon">
              <IconDevice size={20} />
            </span>
            <span className="settings-destination-copy">
              <strong>设备管理</strong>
              <small>查看在线状态、项目数量与同步诊断</small>
            </span>
            <span className="settings-destination-count num">
              {devices.isLoading ? "—" : `${onlineDevices} / ${pairedDevices.length} 在线`}
            </span>
            <IconChevronRight size={17} />
          </button>

          <div className="section-label micro">版本对齐</div>
          <div className={`card version-panel ${versionState}`}>
            <header className="version-panel-head">
              <span className="version-panel-icon">
                <IconShield size={18} />
              </span>
              <span className="version-panel-copy">
                <strong>Client / Server 版本</strong>
                <small>只有版本完全一致时才允许建立业务连接</small>
              </span>
              <span className={`version-state ${versionState}`}>{versionStateLabel}</span>
            </header>
            <div className="version-rows">
              <div className="version-row">
                <span>
                  <strong>Server</strong>
                  <small>当前控制服务</small>
                </span>
                <code>{info.data?.serverVersion ?? "—"}</code>
              </div>
              {activeDevices.length === 0 ? (
                <div className="version-empty">尚无可确认版本的 Client</div>
              ) : (
                activeDevices.map((device) => (
                  <div className="version-row" key={device.id}>
                    <span>
                      <strong>{device.displayName}</strong>
                      <small>Client · {osLabel(device.osFamily, device.architecture)}</small>
                    </span>
                    <span className="version-row-value">
                      <code>{device.agentVersion ?? "未知"}</code>
                      <small className={device.versionCompatibility}>
                        {device.versionCompatibility === "compatible"
                          ? "一致"
                          : device.versionCompatibility === "mismatch"
                            ? `需要 ${device.expectedVersion}`
                            : "待确认"}
                      </small>
                    </span>
                  </div>
                ))
              )}
            </div>
          </div>

          <div className="section-label micro">对话访问级别</div>
          <div className="card access-settings">
            <Segmented
              options={
                [
                  { value: "full", label: "完全访问" },
                  { value: "ask", label: "操作前询问" },
                ] satisfies { value: ConversationAccessMode; label: string }[]
              }
              value={accessMode}
              onChange={setAccessMode}
            />
            <div className={`access-settings-note ${accessMode}`}>
              {accessMode === "full"
                ? "无需批准，可访问系统与网络。"
                : "越过工作区或访问受限资源时询问。"}
            </div>
          </div>

          <div className="section-label micro">设备配对</div>
          <div className="card settings-panel">
            {pairing ? (
              <>
                <div className="pairing-intro">
                  在要接入的电脑上运行，并按提示输入配对码：
                </div>
                <div className="pairing-code">{pairing.code}</div>
                <div className="pairing-expiry">
                  <span className="mono">nuntius-client pair</span> · 配对码 {relTime(pairing.expiresAt)}过期
                </div>
              </>
            ) : (
              <div className="pairing-empty">
                <span className="row-glyph">
                  <IconKey size={17} />
                </span>
                <div className="grow" />
                <button className="btn primary sm" onClick={newPairingCode} disabled={busyPairing}>
                  {busyPairing ? <Spinner sm /> : null}
                  生成配对码
                </button>
              </div>
            )}
          </div>

          <div className="section-label micro">已配对设备</div>
          <div className="list-group">
            {(devices.data ?? []).map((d) => {
              const transient = d.status === "syncing" || d.status === "pairing";
              return (
                <div key={d.id} className="list-row">
                  <Avatar
                    sm
                    text={initials(d.displayName)}
                    tint={tintIndex(d.id)}
                    online={d.status === "online" ? true : d.status === "offline" ? false : undefined}
                  />
                  <div className="grow">
                    <div className="title paired-device-name">{d.displayName}</div>
                    <div className="sub">
                      <span>{osLabel(d.osFamily, d.architecture)}</span>
                      {d.status === "online" || transient ? null : (
                        <span className="num">{relTime(d.lastSeenAt)}在线</span>
                      )}
                    </div>
                  </div>
                  <div className="trailing">
                    {transient ? (
                      <span className="row-state-spinner" role="status" aria-label={statusLabel(d.status)} title={statusLabel(d.status)} />
                    ) : d.status === "degraded" || d.status === "revoked" ? (
                      <span className={`row-state-dot ${d.status}`} role="img" aria-label={statusLabel(d.status)} title={statusLabel(d.status)} />
                    ) : null}
                    {d.status !== "revoked" ? (
                      <>
                        <button className="btn quiet sm" onClick={() => setRenaming(d)}>
                          重命名
                        </button>
                        <button className="btn danger sm" onClick={() => revoke(d.id, d.displayName)}>
                          撤销
                        </button>
                      </>
                    ) : null}
                  </div>
                </div>
              );
            })}
          </div>

          <div className="section-label micro usage-section-label">
            <span>套餐额度</span>
            <button
              className="btn quiet sm usage-refresh"
              onClick={refreshUsage}
              disabled={refreshingUsage}
            >
              <IconRefresh size={14} className={refreshingUsage ? "spinning" : undefined} />
              {refreshingUsage ? "正在通知" : "刷新全部设备"}
            </button>
          </div>
          {providerUsage.isLoading ? (
            <div className="card usage-empty"><Spinner sm />正在读取额度</div>
          ) : usageGroups.length === 0 ? (
            <div className="card usage-empty">
              <span className="usage-empty-mark"><IconBolt size={17} /></span>
              <span>还没有设备额度数据</span>
            </div>
          ) : (
            <div className="usage-device-stack">
              {usageGroups.map(([deviceId, entries], groupIndex) => (
                <section
                  className="usage-device-group"
                  key={deviceId}
                  style={{ "--usage-order": groupIndex } as CSSProperties & Record<"--usage-order", number>}
                >
                  <header className="usage-device-head">
                    <Avatar sm text={initials(entries[0].deviceDisplayName)} tint={tintIndex(deviceId)} />
                    <div>
                      <strong>{entries[0].deviceDisplayName}</strong>
                      <span>{entries.length} 个账户额度</span>
                    </div>
                    <span className="usage-device-time">最新 {relTime(latestReceivedAt(entries))}</span>
                  </header>
                  <div className="usage-card-grid">
                    {entries.map((entry) => <ProviderUsageCard key={entry.report.reportId} entry={entry} />)}
                  </div>
                </section>
              ))}
            </div>
          )}

          <div className="section-label micro">外观</div>
          <div className="card settings-panel">
            <Segmented
              options={
                [
                  { value: "auto", label: "跟随系统" },
                  { value: "light", label: "浅色" },
                  { value: "dark", label: "深色" },
                ] satisfies { value: Theme; label: string }[]
              }
              value={theme}
              onChange={setTheme}
            />
          </div>

          <div className="section-label micro">关于</div>
          <div className="fact-grid">
            <div className="fact">
              <div className="k">服务器版本</div>
              <div className="v num">{info.data?.serverVersion ?? "—"}</div>
            </div>
            <div className="fact">
              <div className="k">传输安全</div>
              <div className="v">
                {info.data?.transportSecurity === "secure"
                  ? "HTTPS / WSS"
                  : info.data?.transportSecurity === "insecure"
                    ? "HTTP（不安全）"
                    : "本地"}
              </div>
            </div>
            <div className="fact">
              <div className="k">API</div>
              <div className="v num">{info.data?.apiVersion ?? "—"}</div>
            </div>
          </div>
        </div>
      </div>
      <RenameDeviceSheet device={renaming} open={renaming !== null} onClose={() => setRenaming(null)} />
      {confirmNode}
    </div>
  );
}

function groupUsageByDevice(entries: ProviderUsageLatestView[]) {
  const groups = new Map<string, ProviderUsageLatestView[]>();
  for (const entry of entries) {
    const group = groups.get(entry.deviceId) ?? [];
    group.push(entry);
    groups.set(entry.deviceId, group);
  }
  return [...groups.entries()];
}

function latestReceivedAt(entries: ProviderUsageLatestView[]) {
  return entries.reduce(
    (latest, entry) => Date.parse(entry.receivedAt) > Date.parse(latest) ? entry.receivedAt : latest,
    entries[0].receivedAt,
  );
}

function ProviderUsageCard({ entry }: { entry: ProviderUsageLatestView }) {
  const { report } = entry;
  const account = report.account;
  const credits = report.credits;
  const plan = report.entitlementPlan ?? account?.plan ?? account?.scope;
  const accountLabel = account?.email ?? account?.externalAccountId ?? "当前账户";
  const healthy = report.status === "ok" || report.status === "partial";
  const expiry = account?.subscriptionExpiresAt ?? account?.credentialExpiresAt;
  const expiryLabel = account?.subscriptionExpiresAt ? "会员到期" : "凭据到期";
  return (
    <article className={`usage-card ${report.provider} usage-${report.status}`}>
      <header className="usage-card-head">
        <span className="usage-provider-mark" aria-hidden="true">
          {report.provider === "codex" ? "O" : "K"}
        </span>
        <div className="usage-account-copy">
          <strong>{report.provider === "codex" ? "OpenAI" : "Kimi Code"}</strong>
          <span title={accountLabel}>{accountLabel}</span>
        </div>
        <span className={`usage-state ${healthy ? "healthy" : "error"}`}>
          {usageStatusLabel(report.status, report.errorCode)}
        </span>
      </header>

      {plan ? <div className="usage-plan-row"><span>{plan}</span><small>{sourceLabel(report.source)}</small></div> : null}

      <div className="usage-window-grid">
        <QuotaMeter label="5 小时" window={report.windows.fiveHour} provider={report.provider} />
        <QuotaMeter label="7 天" window={report.windows.sevenDay} provider={report.provider} />
      </div>

      <footer className="usage-card-foot">
        <div className="usage-meta-row">
          {report.provider === "codex" && credits?.resetCreditsAvailable != null ? (
            <span className="usage-chip accent">
              <IconBolt size={12} />
              重置卡 {credits.resetCreditsAvailable}
              {credits.nextResetCreditExpiresAt ? ` · ${shortDate(credits.nextResetCreditExpiresAt)} 到期` : ""}
            </span>
          ) : null}
          {credits?.balance != null ? (
            <span className="usage-chip">余额 {formatNumber(credits.balance)}</span>
          ) : null}
          {expiry ? <span className="usage-chip"><IconClock size={12} />{expiryLabel} {shortDate(expiry)}</span> : null}
        </div>
        <span className="usage-sampled">采集于 {relTime(report.sampledAt)}</span>
      </footer>
    </article>
  );
}

function QuotaMeter({
  label,
  window,
  provider,
}: {
  label: string;
  window: ProviderQuotaWindow | null;
  provider: "codex" | "kimi" | "pi";
}) {
  const percent = Math.max(0, Math.min(100, window?.usedPercent ?? 0));
  return (
    <div className={`quota-meter ${window ? "" : "missing"}`}>
      <div className="quota-meter-top">
        <span>{label}</span>
        <strong>{window ? `${formatNumber(percent)}%` : "—"}</strong>
      </div>
      <div className="quota-track" aria-label={window ? `${label}已使用 ${formatNumber(percent)}%` : `${label}额度未知`}>
        <span style={{ width: `${percent}%` }} data-provider={provider} />
      </div>
      <div className="quota-meter-meta">
        <span>{quotaAmount(window)}</span>
        <span>{window?.resetsAt ? `重置 ${shortDateTime(window.resetsAt)}` : ""}</span>
      </div>
    </div>
  );
}

function quotaAmount(window: ProviderQuotaWindow | null) {
  if (!window || window.used === null || window.limit === null) return "";
  const remaining = window.remaining === null ? "" : ` · 剩 ${formatNumber(window.remaining)}`;
  return `${formatNumber(window.used)} / ${formatNumber(window.limit)}${remaining}`;
}

function usageStatusLabel(status: string, errorCode: string | null) {
  if (status === "ok") return "已更新";
  if (status === "partial") return "部分更新";
  if (status === "unavailable") return "未登录";
  if (errorCode === "upstream_unauthorized" || errorCode === "credentials_expired") return "登录已失效";
  if (errorCode === "upstream_timeout" || errorCode === "upstream_network") return "网络异常";
  return "获取失败";
}

function sourceLabel(source: string) {
  return ({ oauth: "OAuth", cli: "CLI", api: "API Key", web: "Web", auto: "自动探测" } as Record<string, string>)[source] ?? source;
}

function formatNumber(value: number) {
  return new Intl.NumberFormat("zh-CN", { maximumFractionDigits: value < 100 ? 1 : 0 }).format(value);
}

function shortDate(value: string) {
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? value : new Intl.DateTimeFormat("zh-CN", { month: "numeric", day: "numeric" }).format(date);
}

function shortDateTime(value: string) {
  const date = new Date(value);
  return Number.isNaN(date.getTime())
    ? value
    : new Intl.DateTimeFormat("zh-CN", { month: "numeric", day: "numeric", hour: "2-digit", minute: "2-digit" }).format(date);
}
