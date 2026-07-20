/* Settings: account, pairing codes, device revocation, theme, about. */
import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  Avatar,
  IconKey,
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
} from "@nuntius/shared";
import { api } from "../api";
import { useAccessMode, useSession, useThemeStore } from "../stores";
import { ConnIndicator, TopBar } from "../components";
import { RenameDeviceSheet } from "../sheets/RenameDeviceSheet";

export function SettingsPage() {
  const toast = useToast();
  const qc = useQueryClient();
  const { session, setSession } = useSession();
  const { theme, setTheme } = useThemeStore();
  const { mode: accessMode, setMode: setAccessMode } = useAccessMode();
  const { confirm, node: confirmNode } = useConfirmAction();
  const [pairing, setPairing] = useState<PairingCodeView | null>(null);
  const [busyPairing, setBusyPairing] = useState(false);
  const [renaming, setRenaming] = useState<DeviceSummary | null>(null);

  const info = useQuery({ queryKey: ["info"], queryFn: api.info, staleTime: 60_000 });
  const devices = useQuery({ queryKey: ["devices"], queryFn: api.devices });

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
