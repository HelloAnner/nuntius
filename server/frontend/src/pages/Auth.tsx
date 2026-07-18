/* Login & first-run bootstrap. */
import { useState, type FormEvent } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { Spinner, useToast } from "@nuntius/shared";
import { api, ApiError } from "../api";
import { useSession } from "../stores";

export function AuthPage({ initialized }: { initialized: boolean }) {
  const [mode, setMode] = useState<"login" | "bootstrap">(initialized ? "login" : "bootstrap");
  return (
    <div className="auth-wrap">
      <div className="auth-card">
        <div>
          <div className="auth-logo display">N</div>
          <div className="auth-title display" style={{ marginTop: 18 }}>
            Nuntius
          </div>
          <div className="auth-sub">
            {mode === "login" ? "登录以控制你的设备" : "创建所有者账户，初始化服务器"}
          </div>
        </div>
        {mode === "login" ? <LoginForm /> : <BootstrapForm />}
        {initialized ? (
          <button
            className="btn quiet sm"
            onClick={() => setMode(mode === "login" ? "bootstrap" : "login")}
          >
            {mode === "login" ? "首次使用？初始化服务器" : "返回登录"}
          </button>
        ) : null}
      </div>
    </div>
  );
}

function LoginForm() {
  const toast = useToast();
  const qc = useQueryClient();
  const setSession = useSession((s) => s.setSession);
  const [name, setName] = useState("");
  const [password, setPassword] = useState("");
  const [busy, setBusy] = useState(false);

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    if (busy || !name.trim() || !password) return;
    setBusy(true);
    try {
      const s = await api.login(name.trim(), password);
      setSession(s);
      await qc.invalidateQueries();
    } catch (err) {
      toast(err instanceof ApiError && err.status === 401 ? "用户名或密码错误" : "登录失败，请稍后重试", { error: true });
    } finally {
      setBusy(false);
    }
  };

  return (
    <form onSubmit={submit} style={{ display: "flex", flexDirection: "column", gap: 14 }}>
      <div className="field">
        <label htmlFor="login-name">用户名</label>
        <input
          id="login-name"
          autoComplete="username"
          autoCapitalize="none"
          value={name}
          onChange={(e) => setName(e.target.value)}
        />
      </div>
      <div className="field">
        <label htmlFor="login-pass">密码</label>
        <input
          id="login-pass"
          type="password"
          autoComplete="current-password"
          value={password}
          onChange={(e) => setPassword(e.target.value)}
        />
      </div>
      <button className="btn primary block" disabled={busy || !name.trim() || !password}>
        {busy ? <Spinner sm /> : null}
        登录
      </button>
    </form>
  );
}

function BootstrapForm() {
  const toast = useToast();
  const qc = useQueryClient();
  const setSession = useSession((s) => s.setSession);
  const [token, setToken] = useState("");
  const [name, setName] = useState("");
  const [password, setPassword] = useState("");
  const [busy, setBusy] = useState(false);

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    if (busy || !token.trim() || !name.trim() || password.length < 8) return;
    setBusy(true);
    try {
      const s = await api.bootstrap(token.trim(), name.trim(), password);
      setSession(s);
      await qc.invalidateQueries();
    } catch (err) {
      toast(
        err instanceof ApiError && err.status === 403
          ? "Bootstrap 令牌无效"
          : "初始化失败：" + (err instanceof Error ? err.message : "未知错误"),
        { error: true },
      );
    } finally {
      setBusy(false);
    }
  };

  return (
    <form onSubmit={submit} style={{ display: "flex", flexDirection: "column", gap: 14 }}>
      <div className="notice-banner info" style={{ margin: 0 }}>
        在服务器数据目录中找到 <span className="mono">secrets/bootstrap-token</span> 文件，将其内容粘贴到下方。
      </div>
      <div className="field">
        <label htmlFor="boot-token">Bootstrap 令牌</label>
        <input
          id="boot-token"
          autoCapitalize="none"
          autoCorrect="off"
          value={token}
          onChange={(e) => setToken(e.target.value)}
        />
      </div>
      <div className="field">
        <label htmlFor="boot-name">用户名</label>
        <input
          id="boot-name"
          autoComplete="username"
          autoCapitalize="none"
          value={name}
          onChange={(e) => setName(e.target.value)}
        />
      </div>
      <div className="field">
        <label htmlFor="boot-pass">密码（至少 8 位）</label>
        <input
          id="boot-pass"
          type="password"
          autoComplete="new-password"
          value={password}
          onChange={(e) => setPassword(e.target.value)}
        />
      </div>
      <button
        className="btn primary block"
        disabled={busy || !token.trim() || !name.trim() || password.length < 8}
      >
        {busy ? <Spinner sm /> : null}
        创建并登录
      </button>
    </form>
  );
}
