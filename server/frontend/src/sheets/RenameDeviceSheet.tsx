import { useEffect, useState, type FormEvent } from "react";
import { useQueryClient } from "@tanstack/react-query";
import {
  Sheet,
  Spinner,
  useToast,
  type DeviceSummary,
} from "@nuntius/shared";
import { api } from "../api";

export function RenameDeviceSheet({
  device,
  open,
  onClose,
}: {
  device: DeviceSummary | null;
  open: boolean;
  onClose: () => void;
}) {
  const qc = useQueryClient();
  const toast = useToast();
  const [value, setValue] = useState("");
  const [saving, setSaving] = useState(false);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    if (!open || !device) return;
    setValue(device.displayName);
    setFailed(false);
  }, [device?.displayName, device?.id, open]);

  const normalized = value.trim();
  const byteLength = new TextEncoder().encode(normalized).length;
  const invalid = byteLength === 0 || byteLength > 128;
  const unchanged = normalized === device?.displayName;

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (!device || invalid || unchanged || saving) return;
    setSaving(true);
    setFailed(false);
    try {
      const updated = await api.renameDevice(device.id, normalized);
      qc.setQueryData<DeviceSummary[]>(["devices"], (old) =>
        old?.map((item) => (item.id === updated.id ? updated : item)),
      );
      toast("设备名称已更新");
      onClose();
    } catch {
      setFailed(true);
      toast("重命名失败，请重试", { error: true });
    } finally {
      setSaving(false);
    }
  };

  return (
    <Sheet
      open={open && device !== null}
      onClose={() => {
        if (!saving) onClose();
      }}
      title="重命名设备"
      className="rename-device-sheet"
    >
      <form className="rename-device-form" onSubmit={submit}>
        <p className="rename-device-intro">
          这个名称会显示在所有远程控制页面，并同步到对应电脑的 Nuntius 配置。
        </p>
        <div className="field">
          <label htmlFor="device-display-name">设备名称</label>
          <input
            id="device-display-name"
            value={value}
            onChange={(event) => {
              setValue(event.target.value);
              setFailed(false);
            }}
            onFocus={(event) => event.currentTarget.select()}
            autoComplete="off"
            autoFocus
            maxLength={128}
            aria-invalid={invalid || failed}
            aria-describedby="device-name-help"
          />
          <div
            id="device-name-help"
            className={`rename-device-meta${invalid || failed ? " error" : ""}`}
          >
            <span>
              {failed
                ? "名称未能保存，请检查连接后重试"
                : device?.status === "online"
                  ? "设备在线，将立即写入本机配置"
                  : "设备离线，将在下次连接时自动同步"}
            </span>
            <span className="num">{byteLength}/128</span>
          </div>
        </div>
        <div className="rename-device-actions">
          <button type="button" className="btn ghost block" onClick={onClose} disabled={saving}>
            取消
          </button>
          <button type="submit" className="btn primary block" disabled={invalid || unchanged || saving}>
            {saving ? <Spinner sm /> : null}
            保存名称
          </button>
        </div>
      </form>
    </Sheet>
  );
}
