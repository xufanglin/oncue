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

interface FfmpegProgress {
  downloaded: number;
  total: number;
  fraction: number;
  done: boolean;
  error: string | null;
}

interface Props {
  /** Called after ffmpeg is confirmed ready (download done or already ok). */
  onReady: () => void;
}

export function FfmpegSetup({ onReady }: Props) {
  const [downloading, setDownloading] = useState(false);
  const [progress, setProgress] = useState<FfmpegProgress | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    const unlisten = listen<FfmpegProgress>(
      "ffmpeg:download_progress",
      (ev) => {
        setProgress(ev.payload);
        if (ev.payload.done) {
          setDownloading(false);
          onReady();
        }
        if (ev.payload.error) {
          setDownloading(false);
          setError(ev.payload.error);
        }
      },
    );
    return () => {
      unlisten.then((f) => f());
    };
  }, [onReady]);

  const handleDownload = async () => {
    setError(null);
    setDownloading(true);
    setProgress(null);
    try {
      await invoke("download_ffmpeg");
    } catch (e) {
      setError(String(e));
      setDownloading(false);
    }
  };

  return (
    <Dialog open={true} onOpenChange={() => {}}>
      <DialogContent
        className="sm:max-w-lg"
        onInteractOutside={(e) => e.preventDefault()}
        showCloseButton={false}
      >
        <DialogHeader>
          <DialogTitle>下载 ffmpeg</DialogTitle>
          <DialogDescription>
            处理视频音轨需要 ffmpeg。点击下载会获取 ~21 MB 的 LGPL 静态构建并放入应用数据目录。
            也可以自行 <code>brew install ffmpeg</code> 后重启程序。
          </DialogDescription>
        </DialogHeader>

        {downloading && progress && (
          <div className="flex items-center gap-3">
            <span className="flex-1 text-xs text-muted-foreground">
              ffmpeg ·{" "}
              {progress.total > 0
                ? `${(progress.total / 1024 / 1024).toFixed(0)} MB`
                : `${(progress.downloaded / 1024 / 1024).toFixed(1)} MB`}
            </span>
            <ProgressRing
              value={
                progress.total > 0 ? Math.round(progress.fraction * 100) : 0
              }
            />
          </div>
        )}

        {error && <p className="text-sm text-destructive">{error}</p>}

        <div className="flex justify-end pt-1">
          <Button onClick={handleDownload} disabled={downloading}>
            {downloading ? "下载中…" : "下载 ffmpeg"}
          </Button>
        </div>
      </DialogContent>
    </Dialog>
  );
}
