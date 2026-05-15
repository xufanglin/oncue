import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import { Button } from "@/components/ui/button";
import { RadioGroup, RadioGroupItem } from "@/components/ui/radio-group";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { ProgressRing } from "@/components/ProgressRing";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";

// Bilingual mode is the only translate use-case. We always pair English with
// either Simplified or Traditional Chinese, so the picker only needs two
// presets. The string is sent verbatim to the LLM as the target language hint.
const TARGET_LANGS: { value: string; label: string }[] = [
  { value: "Simplified Chinese", label: "简体中文 + English" },
  { value: "Traditional Chinese", label: "繁體中文 + English" },
];

// ── Types ────────────────────────────────────────────────────────────────────

type Mode = "fast" | "precise" | "generate";

type ProgressEvent =
  | { stage: "extract_audio"; percent: number }
  | { stage: "asr"; percent: number; partial?: string }
  | { stage: "align"; percent: number }
  | { stage: "translate"; current: number; total: number }
  | { stage: "write_output"; done: boolean }
  | { stage: "error"; message: string };

interface SubtitleStream {
  index: number;
  codec: string;
  language: string | null;
  title: string | null;
  forced: boolean;
  default: boolean;
}

function labelForStream(s: SubtitleStream): string {
  const parts: string[] = [];
  parts.push(`#${s.index}`);
  if (s.language) parts.push(s.language);
  if (s.title) parts.push(s.title);
  parts.push(s.codec);
  const tags: string[] = [];
  if (s.default) tags.push("default");
  if (s.forced) tags.push("forced · 仅部分对白");
  return parts.join(" · ") + (tags.length ? ` [${tags.join(", ")}]` : "");
}

// ── Component ────────────────────────────────────────────────────────────────

export function AlignPanel() {
  const [videoPath, setVideoPath] = useState<string | null>(null);
  const [srtPath, setSrtPath] = useState<string | null>(null);
  const [mode, setMode] = useState<Mode>("fast");
  const [targetLang, setTargetLang] = useState("Simplified Chinese");
  const [running, setRunning] = useState(false);
  const [done, setDone] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [percent, setPercent] = useState(0);
  const [indeterminate, setIndeterminate] = useState(false);
  const [status, setStatus] = useState<string>("");
  const [overwritePath, setOverwritePath] = useState<string | null>(null);
  const [embeddedSubs, setEmbeddedSubs] = useState<SubtitleStream[]>([]);
  // null = use Whisper; number = use that embedded subtitle stream index.
  const [embeddedSubChoice, setEmbeddedSubChoice] = useState<number | null>(null);

  // Probe embedded text subtitles when the video changes. Only meaningful
  // for "generate" mode; align modes already require an external srt.
  useEffect(() => {
    if (!videoPath) {
      setEmbeddedSubs([]);
      setEmbeddedSubChoice(null);
      return;
    }
    let cancelled = false;
    invoke<SubtitleStream[]>("list_embedded_subtitles", { videoPath })
      .then((list) => {
        if (cancelled) return;
        setEmbeddedSubs(list);
        // Default pick prefers a "main dialogue" track:
        //   1. skip forced tracks (those only cover foreign-language lines)
        //   2. prefer English among the remaining
        //   3. otherwise take the first non-forced track
        //   4. fall back to whatever exists, including forced
        const nonForced = list.filter((s) => !s.forced);
        const eng = nonForced.find((s) =>
          s.language?.toLowerCase().startsWith("en"),
        );
        const defaultPick = eng ?? nonForced[0] ?? list[0];
        setEmbeddedSubChoice(defaultPick ? defaultPick.index : null);
      })
      .catch(() => {
        if (cancelled) return;
        setEmbeddedSubs([]);
        setEmbeddedSubChoice(null);
      });
    return () => {
      cancelled = true;
    };
  }, [videoPath]);

  // Drag-and-drop video files into the window
  useEffect(() => {
    type DropPayload = { paths: string[]; type: "drop" };
    const VIDEO_EXTS = ["mp4", "mkv", "mov", "avi", "m4v", "webm"];
    const SRT_EXTS = ["srt", "ass", "ssa"];

    const unlisten = listen<DropPayload>("tauri://drag-drop", async (ev) => {
      if (running) return;
      const paths = ev.payload?.paths ?? [];
      const video = paths.find((p) => VIDEO_EXTS.some((e) => p.toLowerCase().endsWith("." + e)));
      const srt = paths.find((p) => SRT_EXTS.some((e) => p.toLowerCase().endsWith("." + e)));
      if (video) {
        setVideoPath(video);
        if (srt) {
          setSrtPath(srt);
          setMode((m) => (m === "generate" ? "fast" : m));
        } else {
          // Probe sibling .srt
          const sibling = await invoke<string | null>("detect_sibling_srt", { videoPath: video });
          if (sibling) {
            setSrtPath(sibling);
            setMode((m) => (m === "generate" ? "fast" : m));
          } else {
            setSrtPath(null);
            setMode("generate");
          }
        }
      } else if (srt) {
        setSrtPath(srt);
      }
    });
    return () => {
      unlisten.then((f) => f());
    };
  }, [running]);

  useEffect(() => {
    const unlisten = listen<ProgressEvent>("pipeline:progress", (ev) => {
      const p = ev.payload;

      if (p.stage === "extract_audio") {
        setPercent(p.percent / 4);
        setIndeterminate(false);
        setStatus(`提取音轨中… ${p.percent}%`);
      } else if (p.stage === "asr") {
        setPercent(25 + p.percent / 2);
        // Whisper's `full()` call does not emit incremental progress on
        // Metal, so within a single ASR run we get only 0% then 100% — show
        // a spinner instead of a stuck "0%".
        setIndeterminate(p.percent < 100);
        setStatus(
          p.partial
            ? `语音识别：${p.partial}`
            : "语音识别中…",
        );
      } else if (p.stage === "align") {
        setPercent(75 + p.percent / 4);
        setIndeterminate(false);
        setStatus(`对齐计算中… ${p.percent}%`);
      } else if (p.stage === "translate") {
        const pct = p.total > 0 ? (p.current / p.total) * 100 : 0;
        setPercent(75 + pct * 0.2);
        setIndeterminate(false);
        setStatus(`翻译中… ${Math.round(pct)}%`);
      } else if (p.stage === "write_output") {
        setPercent(p.done ? 100 : 95);
        setIndeterminate(false);
        if (p.done) {
          setStatus("已完成");
          setDone(true);
          setRunning(false);
        } else {
          setStatus("写入文件中…");
        }
      } else if (p.stage === "error") {
        setStatus("");
        setIndeterminate(false);
        setError(p.message);
        setRunning(false);
      }
    });
    return () => {
      unlisten.then((f) => f());
    };
  }, []);

  const pickVideo = async () => {
    const path = await open({
      filters: [{ name: "视频", extensions: ["mp4", "mkv", "mov", "avi", "m4v"] }],
    });
    if (typeof path === "string") setVideoPath(path);
  };

  const pickSrt = async () => {
    const path = await open({
      filters: [{ name: "字幕", extensions: ["srt", "ass", "ssa"] }],
    });
    if (typeof path === "string") setSrtPath(path);
  };

  const needsSrt = mode !== "generate";

  // Backend's `OutputExists` error string format: "output already exists: <path>"
  const OUTPUT_EXISTS_PREFIX = "output already exists:";

  const runPipeline = async (force: boolean) => {
    setRunning(true);
    setDone(false);
    setError(null);
    setPercent(0);
    setIndeterminate(false);
    setStatus("启动…");
    try {
      if (mode === "fast") {
        await invoke("start_resync_fast", { videoPath, srtPath });
      } else if (mode === "precise") {
        await invoke("start_resync_precise", { videoPath, srtPath });
      } else {
        await invoke("start_generate", {
          videoPath,
          targetLang,
          force,
          embeddedSubtitleIndex: embeddedSubChoice,
        });
      }
    } catch (e) {
      const msg = String(e);
      // Generate-mode only: ask user before overwriting an existing srt.
      const m = msg.match(new RegExp(OUTPUT_EXISTS_PREFIX + "\\s*(.+)$"));
      if (mode === "generate" && !force && m) {
        setOverwritePath(m[1].trim());
        setRunning(false);
        setStatus("");
        return;
      }
      setError(msg);
      setStatus("");
      setRunning(false);
    }
  };

  const handleStart = async () => {
    if (!videoPath) return;
    if (needsSrt && !srtPath) return;
    await runPipeline(false);
  };

  const handleConfirmOverwrite = async () => {
    setOverwritePath(null);
    await runPipeline(true);
  };

  const handleCancel = async () => {
    await invoke("cancel_pipeline");
    setStatus("已取消");
    setRunning(false);
  };

  const canStart =
    !!videoPath &&
    (!needsSrt || !!srtPath) &&
    (mode !== "generate" || targetLang.trim().length > 0) &&
    !running;

  return (
    <div className="w-full max-w-xl space-y-4">
      <Card>
        <CardHeader>
          <CardTitle>字幕处理</CardTitle>
        </CardHeader>
        <CardContent className="space-y-4">
          <div className="space-y-2">
            <div className="flex items-center gap-2">
              <Button variant="outline" size="sm" onClick={pickVideo} disabled={running}>
                选择视频
              </Button>
              <span className="truncate text-sm text-muted-foreground max-w-xs">
                {videoPath ?? "未选择"}
              </span>
            </div>
            {needsSrt && (
              <div className="flex items-center gap-2">
                <Button variant="outline" size="sm" onClick={pickSrt} disabled={running}>
                  选择字幕
                </Button>
                <span className="truncate text-sm text-muted-foreground max-w-xs">
                  {srtPath ?? "未选择"}
                </span>
              </div>
            )}
          </div>

          <RadioGroup
            value={mode}
            onValueChange={(v) => setMode(v as Mode)}
            className="flex flex-col gap-2"
            disabled={running}
          >
            <div className="flex items-center gap-2">
              <RadioGroupItem value="fast" id="mode-fast" />
              <label htmlFor="mode-fast" className="cursor-pointer text-sm">
                快档对齐（仅恒定偏移，≤15s）
              </label>
            </div>
            <div className="flex items-center gap-2">
              <RadioGroupItem value="precise" id="mode-precise" />
              <label htmlFor="mode-precise" className="cursor-pointer text-sm">
                精档对齐（逐词对齐，≤30min）
              </label>
            </div>
            <div className="flex items-center gap-2">
              <RadioGroupItem value="generate" id="mode-generate" />
              <label htmlFor="mode-generate" className="cursor-pointer text-sm">
                生成双语字幕（ASR + 翻译）
              </label>
            </div>
          </RadioGroup>

          {mode === "generate" && (
            <div className="space-y-3">
              <div className="space-y-1">
                <label className="text-xs text-muted-foreground">翻译目标语言</label>
                <Select value={targetLang} onValueChange={setTargetLang} disabled={running}>
                  <SelectTrigger className="w-full">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    {TARGET_LANGS.map((l) => (
                      <SelectItem key={l.value} value={l.value}>
                        {l.label}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </div>

              {embeddedSubs.length > 0 && (
                <div className="space-y-1">
                  <label className="text-xs text-muted-foreground">
                    源字幕（视频已内嵌文本字幕，可跳过 Whisper）
                  </label>
                  <Select
                    value={embeddedSubChoice === null ? "asr" : String(embeddedSubChoice)}
                    onValueChange={(v) =>
                      setEmbeddedSubChoice(v === "asr" ? null : Number(v))
                    }
                    disabled={running}
                  >
                    <SelectTrigger className="w-full">
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent>
                      {embeddedSubs.map((s) => (
                        <SelectItem key={s.index} value={String(s.index)}>
                          {labelForStream(s)}
                        </SelectItem>
                      ))}
                      <SelectItem value="asr">使用 Whisper 重新识别</SelectItem>
                    </SelectContent>
                  </Select>
                </div>
              )}
            </div>
          )}

          {(running || done) && (
            <div className="flex items-center gap-3">
              <p className="flex-1 text-sm text-muted-foreground truncate">
                {status || (done ? "已完成" : "运行中…")}
              </p>
              <ProgressRing value={percent} indeterminate={indeterminate} />
            </div>
          )}

          {error && (
            <p className="text-sm text-destructive">{error}</p>
          )}

          <div className="flex gap-2">
            <Button onClick={handleStart} disabled={!canStart}>
              开始
            </Button>
            {running && (
              <Button variant="outline" onClick={handleCancel}>
                取消
              </Button>
            )}
          </div>
        </CardContent>
      </Card>

      <Dialog
        open={overwritePath !== null}
        onOpenChange={(o) => !o && setOverwritePath(null)}
      >
        <DialogContent className="sm:max-w-md">
          <DialogHeader>
            <DialogTitle>字幕已存在</DialogTitle>
            <DialogDescription>
              目标字幕文件已存在。继续会把原文件重命名为 <code>.srt.bak</code> 后再生成新字幕。
            </DialogDescription>
          </DialogHeader>
          {overwritePath && (
            <p className="text-xs text-muted-foreground break-all">
              {overwritePath}
            </p>
          )}
          <DialogFooter>
            <Button variant="outline" onClick={() => setOverwritePath(null)}>
              取消
            </Button>
            <Button onClick={handleConfirmOverwrite}>继续</Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
