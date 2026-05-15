import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { Settings as SettingsIcon } from "lucide-react";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { RadioGroup, RadioGroupItem } from "@/components/ui/radio-group";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { ProgressRing } from "@/components/ProgressRing";

// ── Types matching Rust ProviderConfig ───────────────────────────────────────

type ProviderType =
  | "openai_official"
  | "anthropic_official"
  | "openai_compatible"
  | "anthropic_compatible";

type ProviderConfig =
  | { type: "openai_official"; api_key: string; model: string }
  | { type: "anthropic_official"; api_key: string; model: string }
  | {
      type: "openai_compatible";
      base_url: string;
      api_key: string;
      model: string;
    }
  | {
      type: "anthropic_compatible";
      base_url: string;
      api_key: string;
      model: string;
    };

type Providers = {
  openai_official?: ProviderConfig;
  anthropic_official?: ProviderConfig;
  openai_compatible?: ProviderConfig;
  anthropic_compatible?: ProviderConfig;
  active?: ProviderType;
};

interface ModelStatus {
  name: string;
  label: string;
  size_bytes: number;
  present: boolean;
  disk_bytes: number | null;
  description: string;
}

interface ModelDownloadProgress {
  name: string;
  downloaded: number;
  total: number;
  fraction: number;
  done: boolean;
  error: string | null;
}

interface FfmpegProgress {
  downloaded: number;
  total: number;
  fraction: number;
  done: boolean;
  error: string | null;
}

interface SystemStatus {
  ffmpeg_ok: boolean;
  ffmpeg_version: string | null;
  model_ready: boolean;
}

const TYPE_LABELS: Record<ProviderType, string> = {
  openai_official: "OpenAI 官方",
  anthropic_official: "Anthropic 官方",
  openai_compatible: "OpenAI 兼容",
  anthropic_compatible: "Anthropic 兼容",
};

const DEFAULT_MODEL: Record<ProviderType, string> = {
  openai_official: "gpt-4o-mini",
  anthropic_official: "claude-haiku-4-5",
  openai_compatible: "",
  anthropic_compatible: "",
};

const NEEDS_BASE_URL = (t: ProviderType) =>
  t === "openai_compatible" || t === "anthropic_compatible";

// ── Component ────────────────────────────────────────────────────────────────

export function SettingsDialog() {
  const [open, setOpen] = useState(false);

  // Long-running downloads (model & ffmpeg) outlive tab switches and even
  // dialog closes, so we hoist their state here. Otherwise switching tabs
  // would unmount the tab component, lose the "downloading" flag, and let
  // the user kick off a duplicate download.
  const [modelDownload, setModelDownload] =
    useState<ModelDownloadProgress | null>(null);
  const [ffmpegDownload, setFfmpegDownload] = useState<FfmpegProgress | null>(
    null,
  );

  useEffect(() => {
    const u1 = listen<ModelDownloadProgress>(
      "model:download_progress",
      (ev) => {
        setModelDownload(ev.payload);
        if (ev.payload.done || ev.payload.error) {
          setTimeout(() => setModelDownload(null), 800);
        }
      },
    );
    const u2 = listen<FfmpegProgress>("ffmpeg:download_progress", (ev) => {
      setFfmpegDownload(ev.payload);
      if (ev.payload.done || ev.payload.error) {
        setTimeout(() => setFfmpegDownload(null), 800);
      }
    });
    return () => {
      u1.then((f) => f());
      u2.then((f) => f());
    };
  }, []);

  return (
    <Dialog open={open} onOpenChange={setOpen}>
      <DialogTrigger asChild>
        <Button variant="outline" size="sm" className="gap-1.5">
          <SettingsIcon className="size-4" />
          设置
        </Button>
      </DialogTrigger>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>设置</DialogTitle>
        </DialogHeader>

        <Tabs defaultValue="provider">
          <TabsList className="grid w-full grid-cols-3">
            <TabsTrigger value="provider">翻译 Provider</TabsTrigger>
            <TabsTrigger value="model">Whisper 模型</TabsTrigger>
            <TabsTrigger value="ffmpeg">ffmpeg</TabsTrigger>
          </TabsList>
          <TabsContent value="provider" className="pt-3">
            <ProviderTab open={open} />
          </TabsContent>
          <TabsContent value="model" className="pt-3">
            <ModelTab open={open} progress={modelDownload} />
          </TabsContent>
          <TabsContent value="ffmpeg" className="pt-3">
            <FfmpegTab open={open} progress={ffmpegDownload} />
          </TabsContent>
        </Tabs>
      </DialogContent>
    </Dialog>
  );
}

// ── Provider tab ─────────────────────────────────────────────────────────────

function ProviderTab({ open }: { open: boolean }) {
  const [type, setType] = useState<ProviderType>("openai_official");
  const [apiKey, setApiKey] = useState("");
  const [baseUrl, setBaseUrl] = useState("");
  const [model, setModel] = useState(DEFAULT_MODEL.openai_official);
  const [testing, setTesting] = useState(false);
  const [saving, setSaving] = useState(false);
  const [message, setMessage] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!open) return;
    invoke<Providers>("get_providers").then((p) => {
      const active = p.active ?? "openai_official";
      setType(active);
      loadInto(active, p);
    });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);

  const loadInto = (t: ProviderType, p: Providers) => {
    const cfg = p[t];
    if (cfg) {
      setApiKey("api_key" in cfg ? cfg.api_key : "");
      setBaseUrl("base_url" in cfg ? cfg.base_url : "");
      setModel(cfg.model || DEFAULT_MODEL[t]);
    } else {
      setApiKey("");
      setBaseUrl("");
      setModel(DEFAULT_MODEL[t]);
    }
    setMessage(null);
    setError(null);
  };

  const handleTypeChange = async (t: string) => {
    const next = t as ProviderType;
    setType(next);
    const p = await invoke<Providers>("get_providers");
    loadInto(next, p);
  };

  const buildConfig = (): ProviderConfig => {
    if (type === "openai_official") return { type, api_key: apiKey, model };
    if (type === "anthropic_official") return { type, api_key: apiKey, model };
    return { type, base_url: baseUrl, api_key: apiKey, model };
  };

  const handleTest = async () => {
    setTesting(true);
    setError(null);
    setMessage(null);
    try {
      await invoke("test_provider", { config: buildConfig() });
      setMessage("✓ 连接成功");
    } catch (e) {
      setError(String(e));
    } finally {
      setTesting(false);
    }
  };

  const handleSave = async () => {
    setSaving(true);
    setError(null);
    setMessage(null);
    try {
      await invoke("save_provider", { config: buildConfig() });
      setMessage("✓ 已保存");
    } catch (e) {
      setError(String(e));
    } finally {
      setSaving(false);
    }
  };

  const canSubmit =
    apiKey.trim().length > 0 &&
    model.trim().length > 0 &&
    (!NEEDS_BASE_URL(type) || baseUrl.trim().length > 0);

  return (
    <div className="space-y-4">
      <RadioGroup
        value={type}
        onValueChange={handleTypeChange}
        className="grid grid-cols-2 gap-2"
      >
        {(Object.keys(TYPE_LABELS) as ProviderType[]).map((t) => (
          <div key={t} className="flex items-center gap-2">
            <RadioGroupItem value={t} id={`pt-${t}`} />
            <label htmlFor={`pt-${t}`} className="text-sm cursor-pointer">
              {TYPE_LABELS[t]}
            </label>
          </div>
        ))}
      </RadioGroup>

      {NEEDS_BASE_URL(type) && (
        <div className="space-y-1">
          <label className="text-xs text-muted-foreground">Base URL</label>
          <Input
            value={baseUrl}
            onChange={(e) => setBaseUrl(e.target.value)}
            placeholder="https://api.example.com/v1"
          />
        </div>
      )}

      <div className="space-y-1">
        <label className="text-xs text-muted-foreground">API Key</label>
        <Input
          type="password"
          value={apiKey}
          onChange={(e) => setApiKey(e.target.value)}
          placeholder="sk-…"
        />
      </div>

      <div className="space-y-1">
        <label className="text-xs text-muted-foreground">Model</label>
        <Input
          value={model}
          onChange={(e) => setModel(e.target.value)}
          placeholder={DEFAULT_MODEL[type] || "model id"}
        />
      </div>

      {message && <p className="text-sm text-emerald-600">{message}</p>}
      {error && <p className="text-sm text-destructive">{error}</p>}

      <div className="flex justify-end gap-2 pt-2">
        <Button
          variant="outline"
          size="sm"
          disabled={!canSubmit || testing}
          onClick={handleTest}
        >
          {testing ? "测试中…" : "测试连接"}
        </Button>
        <Button size="sm" disabled={!canSubmit || saving} onClick={handleSave}>
          {saving ? "保存中…" : "保存"}
        </Button>
      </div>
    </div>
  );
}

// ── Model tab ────────────────────────────────────────────────────────────────

function ModelTab({
  open,
  progress,
}: {
  open: boolean;
  progress: ModelDownloadProgress | null;
}) {
  const [models, setModels] = useState<ModelStatus[]>([]);
  const [active, setActive] = useState<string>("");
  const [error, setError] = useState<string | null>(null);

  const refresh = async () => {
    const list = await invoke<ModelStatus[]>("list_models");
    setModels(list);
    const cur = await invoke<string | null>("get_active_model");
    if (cur) setActive(cur);
    else if (list.find((m) => m.present))
      setActive(list.find((m) => m.present)!.name);
  };

  useEffect(() => {
    if (!open) return;
    refresh();
  }, [open]);

  // Refresh model list when a download finishes so the row flips to "已下载".
  useEffect(() => {
    if (progress?.done) refresh();
    if (progress?.error) setError(progress.error);
  }, [progress?.done, progress?.error]);

  // The currently downloading model name, or null. Driven by the hoisted
  // progress state so it survives tab switches.
  const downloading =
    progress && !progress.done && !progress.error ? progress.name : null;

  const handleDownload = async (name: string) => {
    if (downloading) return; // guard against double-click during tab switches
    setError(null);
    try {
      await invoke("download_model", { name });
    } catch (e) {
      setError(String(e));
    }
  };

  const handleSelect = async (name: string) => {
    setActive(name);
    try {
      await invoke("set_active_model", { name });
    } catch (e) {
      setError(String(e));
    }
  };

  return (
    <div className="space-y-3">
      <p className="text-xs text-muted-foreground">
        选择已下载的模型作为活动模型，未下载的可点击下载。
      </p>
      <div className="space-y-2">
        {models.map((m) => {
          const isActive = active === m.name;
          const isDownloading = downloading === m.name;
          return (
            <div
              key={m.name}
              className="flex items-center justify-between rounded-md border p-2 text-sm"
            >
              <div className="flex items-start gap-3 flex-1 min-w-0">
                <input
                  type="radio"
                  checked={isActive}
                  disabled={!m.present}
                  onChange={() => handleSelect(m.name)}
                  className="mt-0.5 h-4 w-4"
                />
                <div className="min-w-0">
                  <div className="font-medium">
                    {m.label}
                    <span className="ml-2 text-xs font-normal text-muted-foreground">
                      {m.present ? "已下载" : "未下载"}
                    </span>
                  </div>
                  <div className="text-xs text-muted-foreground mt-0.5">
                    {m.description}
                  </div>
                </div>
              </div>
              {!m.present ? (
                <Button
                  size="sm"
                  variant="outline"
                  disabled={!!downloading}
                  onClick={() => handleDownload(m.name)}
                >
                  {isDownloading ? "下载中…" : "下载"}
                </Button>
              ) : isActive ? (
                <span className="inline-flex items-center rounded-md border px-2.5 py-1 text-xs font-medium leading-none whitespace-nowrap">
                  激活
                </span>
              ) : null}
            </div>
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
    </div>
  );
}

// ── ffmpeg tab ───────────────────────────────────────────────────────────────

function FfmpegTab({
  open,
  progress,
}: {
  open: boolean;
  progress: FfmpegProgress | null;
}) {
  const [status, setStatus] = useState<SystemStatus | null>(null);
  const [error, setError] = useState<string | null>(null);

  const refresh = async () => {
    const s = await invoke<SystemStatus>("system_check");
    setStatus(s);
  };

  useEffect(() => {
    if (!open) return;
    refresh();
  }, [open]);

  useEffect(() => {
    if (progress?.done) refresh();
    if (progress?.error) setError(progress.error);
  }, [progress?.done, progress?.error]);

  const downloading = !!progress && !progress.done && !progress.error;

  const handleDownload = async () => {
    if (downloading) return;
    setError(null);
    try {
      await invoke("download_ffmpeg");
    } catch (e) {
      setError(String(e));
    }
  };

  return (
    <div className="space-y-3">
      <div className="space-y-1.5 text-sm">
        <div className="font-medium">
          状态：{status?.ffmpeg_ok ? "已就绪" : "未就绪"}
        </div>
        {status?.ffmpeg_version && (
          <div className="text-xs text-muted-foreground truncate">
            {status.ffmpeg_version}
          </div>
        )}
      </div>

      <p className="text-xs text-muted-foreground">
        {status?.ffmpeg_ok
          ? "已检测到可用的 ffmpeg。重新下载会覆盖应用数据目录中的版本。"
          : "未检测到系统或应用内 ffmpeg。点击下载会获取约 21 MB 的 LGPL 静态构建。"}
      </p>

      {downloading && progress && (
        <div className="flex items-center gap-3">
          <span className="flex-1 text-xs text-muted-foreground">
            ffmpeg ·{" "}
            {progress.total > 0
              ? `${(progress.total / 1024 / 1024).toFixed(0)} MB`
              : `${(progress.downloaded / 1024 / 1024).toFixed(1)} MB`}
          </span>
          <ProgressRing
            value={progress.total > 0 ? Math.round(progress.fraction * 100) : 0}
          />
        </div>
      )}
      {error && <p className="text-sm text-destructive">{error}</p>}

      <div className="flex justify-end">
        <Button size="sm" disabled={downloading} onClick={handleDownload}>
          {downloading
            ? "下载中…"
            : status?.ffmpeg_ok
              ? "重新下载"
              : "下载 ffmpeg"}
        </Button>
      </div>
    </div>
  );
}
