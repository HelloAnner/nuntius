import { useQueryClient } from "@tanstack/react-query";
import { DirectoryBrowser, Sheet, newIdemKey, useToast } from "@nuntius/shared";
import { api } from "../api";
import { trackCommand } from "../events";

export function DirectoryPicker({
  deviceId,
  open,
  onClose,
}: {
  deviceId: string;
  open: boolean;
  onClose: () => void;
}) {
  const toast = useToast();
  const qc = useQueryClient();
  const create = async (directoryRef: string, displayName: string) => {
    const idemKey = newIdemKey();
    try {
      const receipt = await api.createProject(
        deviceId,
        directoryRef,
        displayName,
        idemKey,
      );
      trackCommand(qc, receipt.commandId, undefined, "project.create");
      toast("项目创建命令已送达设备");
      void Promise.all([
        qc.invalidateQueries({ queryKey: ["projects", deviceId] }),
        qc.invalidateQueries({ queryKey: ["devices"] }),
      ]);
      onClose();
    } catch (e) {
      toast(e instanceof Error ? e.message : "创建失败", { error: true });
      throw e;
    }
  };

  return (
    <Sheet open={open} onClose={onClose} title="添加项目" className="directory-sheet">
      <DirectoryBrowser
        loadRoots={() => api.directoryRoots(deviceId)}
        loadDirectory={(parentRef, cursor) => api.directories(deviceId, parentRef, cursor)}
        onCreate={(entry, name) => create(entry.directoryRef, name)}
        emptyHint="这个目录中没有子文件夹"
      />
    </Sheet>
  );
}
