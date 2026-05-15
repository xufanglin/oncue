import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { ModelSetupDialog } from "@/components/ModelSetupDialog";
import { AlignPanel } from "@/components/AlignPanel";
import { SettingsDialog } from "@/components/SettingsDialog";
import { FfmpegSetup } from "@/components/FfmpegSetup";

interface SystemStatus {
  ffmpeg_ok: boolean;
  ffmpeg_version: string | null;
  model_ready: boolean;
}

function App() {
  const [ready, setReady] = useState(false);
  const [sys, setSys] = useState<SystemStatus | null>(null);

  const refreshStatus = useCallback(() => {
    invoke<SystemStatus>("system_check").then(setSys);
  }, []);

  useEffect(() => {
    refreshStatus();
  }, [refreshStatus]);

  return (
    <main className="flex min-h-screen flex-col items-center justify-start bg-background text-foreground p-6">
      <ModelSetupDialog onReady={() => setReady(true)} />
      {ready && (
        <>
          <div className="relative w-full max-w-xl mb-4 flex items-center justify-center">
            <h1 className="text-xl font-semibold">OnCue</h1>
            <div className="absolute right-0">
              <SettingsDialog />
            </div>
          </div>

          {sys && !sys.ffmpeg_ok && <FfmpegSetup onReady={refreshStatus} />}

          <AlignPanel />
        </>
      )}
    </main>
  );
}

export default App;
