import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { ProgressRing } from "@/components/ProgressRing";

interface ModelStatus {
  name: string;
  label: string;
  size_bytes: number;
  present: boolean;
  disk_bytes: number | null;
  description: string;
}

interface DownloadProgress {
  name: string;
  downloaded: number;
  total: number;
  fraction: number;
  done: boolean;
  error: string | null;
}

interface Props {
  onReady: () => void;
}

export function ModelSetupDialog({ onReady }: Props) {
  const [models, setModels] = useState<ModelStatus[]>([]);
  const [selected, setSelected] = useState<string>("");
  const [downloading, setDownloading] = useState(false);
  const [progress, setProgress] = useState<DownloadProgress | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [open, setOpen] = useState(false);

  useEffect(() => {
    invoke<ModelStatus[]>("list_models").then((list) => {
      setModels(list);
      if (list.some((m) => m.present)) {
        onReady();
        return;
      }
      setSelected(list[0]?.name ?? "");
      setOpen(true);
    });
  }, [onReady]);

  useEffect(() => {
    const unlisten = listen<DownloadProgress>(
      "model:download_progress",
      (event) => {
        setProgress(event.payload);
        if (event.payload.done) {
          setDownloading(false);
          setOpen(false);
          onReady();
        }
        if (event.payload.error) {
          setDownloading(false);
          setError(event.payload.error);
        }
      },
    );
    return () => {
      unlisten.then((f) => f());
    };
  }, [onReady]);

  const handleDownload = async () => {
    if (!selected) return;
    setError(null);
    setDownloading(true);
    setProgress(null);
    try {
      await invoke("download_model", { name: selected });
    } catch (e) {
      setError(String(e));
      setDownloading(false);
    }
  };

  return (
    <Dialog open={open} onOpenChange={() => {}}>
      <DialogContent
        className="sm:max-w-lg"
        onInteractOutside={(e) => e.preventDefault()}
      >
        <DialogHeader>
          <DialogTitle>下载 Whisper 模型</DialogTitle>
          <DialogDescription>
            首次使用需要下载语音识别模型。模型越大精度越高，速度越慢。
          </DialogDescription>
        </DialogHeader>

        <div className="space-y-2 pt-1">
          {models.map((m) => {
            const isSelected = selected === m.name;
            return (
              <button
                key={m.name}
                type="button"
                disabled={downloading}
                onClick={() => setSelected(m.name)}
                className={`flex w-full items-start gap-3 rounded-md border p-2 text-left text-sm transition-colors ${
                  isSelected ? "border-foreground" : "hover:bg-muted/40"
                } disabled:opacity-60`}
              >
                <input
                  type="radio"
                  checked={isSelected}
                  readOnly
                  disabled={downloading}
                  className="mt-0.5 h-4 w-4 shrink-0"
                />
                <div className="min-w-0">
                  <div className="font-medium">{m.label}</div>
                  <div className="mt-0.5 text-xs text-muted-foreground">
                    {m.description}
                  </div>
                </div>
              </button>
            );
          })}
        </div>

        {downloading && progress && (
          <div className="flex items-center gap-3">
            <span className="flex-1 text-xs text-muted-foreground">
              {progress.name} · {(progress.total / 1024 / 1024).toFixed(0)} MB
            </span>
            <ProgressRing value={Math.round(progress.fraction * 100)} />
          </div>
        )}

        {error && <p className="text-sm text-destructive">{error}</p>}

        <div className="flex justify-end pt-1">
          <Button onClick={handleDownload} disabled={downloading || !selected}>
            {downloading ? "下载中…" : "开始下载"}
          </Button>
        </div>
      </DialogContent>
    </Dialog>
  );
}
