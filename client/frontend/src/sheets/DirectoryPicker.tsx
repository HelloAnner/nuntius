import { useQueryClient } from "@tanstack/react-query";
import { DirectoryBrowser, Sheet, useToast } from "@nuntius/shared";
import { api } from "../api";

export function DirectoryPicker({
  open,
  onClose,
}: {
  open: boolean;
  onClose: () => void;
}) {
  const toast = useToast();
  const qc = useQueryClient();
  const create = async (directoryRef: string, displayName: string) => {
    try {
      await api.createProject(directoryRef, displayName);
      toast("项目已创建");
      void Promise.all([
        qc.invalidateQueries({ queryKey: ["projects"] }),
        qc.invalidateQueries({ queryKey: ["info"] }),
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
        loadRoots={api.directoryRoots}
        loadDirectory={api.directories}
        onCreate={(entry, name) => create(entry.directoryRef, name)}
        emptyHint="这个目录中没有子文件夹"
      />
    </Sheet>
  );
}
