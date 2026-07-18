/* Settings: account, pairing codes, device revocation, theme, about. */
import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  Avatar,
  IconKey,
  IconMoon,
  IconSun,
  Segmented,
  Spinner,
  deviceTone,
  fullTime,
  initials,
  osLabel,
  relTime,
  statusLabel,
  tintIndex,
  useConfirmAction,
  useToast,
  Pill,
  type PairingCodeView,
  type Theme,
} from "@nuntius/shared";
import { api } from "../api";
import { useSession, useThemeStore } from "../stores";
import { ConnIndicator, TopBar } from "../components";

export function SettingsPage() {
  const toast = useToast();
  const qc = useQueryClient();
  const { session, setSession } = useSession();
  const { theme, setTheme } = useThemeStore();
  const { confirm, node: confirmNode } = useConfirmAction();
  const [pairing, setPairing] = useState<PairingCodeView | null>(null);
  const [busyPairing, setBusyPairing] = useState(false);

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
    <div className="page">
      <TopBar title="设置" trailing={<ConnIndicator />} />
      <div className="page-scroll">
        <div className="page-col" style={{ maxWidth: 640 }}>
          <div className="card settings-user">
            <Avatar text={initials(session?.loginName ?? "?")} tint={1} />
            <div className="grow" style={{ flex: 1, minWidth: 0 }}>
              <div className="title" style={{ fontSize: 16, fontWeight: 600 }}>
                {session?.loginName}
              </div>
              <div className="sub num">会话有效期至 {fullTime(session?.expiresAt)}</div>
            </div>
            <button className="btn ghost sm" onClick={logout}>
              退出
            </button>
          </div>

          <div className="section-label micro">设备配对</div>
          <div className="card" style={{ padding: 16 }}>
            {pairing ? (
              <>
                <div style={{ fontSize: 13, color: "var(--ink-2)" }}>
                  在要接入的电脑上运行，并按提示输入配对码：
                </div>
                <div className="pairing-code">{pairing.code}</div>
                <div style={{ fontSize: 12.5, color: "var(--ink-3)", textAlign: "center" }}>
                  <span className="mono">nuntius-client pair</span> · 配对码 {relTime(pairing.expiresAt)}过期
                </div>
              </>
            ) : (
              <div style={{ display: "flex", alignItems: "center", gap: 12 }}>
                <span className="row-glyph">
                  <IconKey size={17} />
                </span>
                <div style={{ flex: 1, fontSize: 13.5, color: "var(--ink-2)", lineHeight: 1.6 }}>
                  生成一次性配对码，把新的电脑接入你的账户。
                </div>
                <button className="btn primary sm" onClick={newPairingCode} disabled={busyPairing}>
                  {busyPairing ? <Spinner sm /> : null}
                  生成配对码
                </button>
              </div>
            )}
          </div>

          <div className="section-label micro">已配对设备</div>
          <div className="list-group">
            {(devices.data ?? []).map((d) => (
              <div key={d.id} className="list-row">
                <Avatar sm text={initials(d.displayName)} tint={tintIndex(d.id)} online={d.status === "online"} />
                <div className="grow">
                  <div className="title" style={{ fontSize: 14.5 }}>{d.displayName}</div>
                  <div className="sub">
                    <span>{osLabel(d.osFamily, d.architecture)}</span>
                    <span>·</span>
                    <span className="num">{relTime(d.lastSeenAt)}在线</span>
                  </div>
                </div>
                <div className="trailing">
                  <Pill tone={deviceTone(d.status)}>{statusLabel(d.status)}</Pill>
                  {d.status !== "revoked" ? (
                    <button className="btn danger sm" onClick={() => revoke(d.id, d.displayName)}>
                      撤销
                    </button>
                  ) : null}
                </div>
              </div>
            ))}
          </div>

          <div className="section-label micro">外观</div>
          <div className="card" style={{ padding: 16 }}>
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
            <div style={{ display: "flex", gap: 16, marginTop: 12, fontSize: 12, color: "var(--ink-3)" }}>
              <span style={{ display: "inline-flex", alignItems: "center", gap: 5 }}>
                <IconSun size={13} /> 纸面浅色系
              </span>
              <span style={{ display: "inline-flex", alignItems: "center", gap: 5 }}>
                <IconMoon size={13} /> 墨色深色系
              </span>
            </div>
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
          <p style={{ margin: "26px 0 10px", textAlign: "center", fontSize: 12, color: "var(--ink-4)" }}>
            Nuntius · 你的多设备 Codex 控制平面
          </p>
        </div>
      </div>
      {confirmNode}
    </div>
  );
}
