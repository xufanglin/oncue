interface Props {
  /** 0-100; ignored when `indeterminate` is true. */
  value: number;
  size?: number;
  stroke?: number;
  /** Optional label rendered inside the ring (defaults to "{value}%"). */
  label?: string;
  /**
   * Show a continuously rotating partial arc instead of a percent. Useful for
   * stages where granular progress isn't available (e.g. a single Whisper
   * `full()` call on Metal where there are no callbacks during inference).
   */
  indeterminate?: boolean;
}

export function ProgressRing({
  value,
  size = 36,
  stroke = 4,
  label,
  indeterminate = false,
}: Props) {
  const r = (size - stroke) / 2;
  const c = 2 * Math.PI * r;
  const v = Math.max(0, Math.min(100, value));
  const dash = indeterminate ? c * 0.25 : (v / 100) * c;
  const text = indeterminate ? (label ?? "…") : (label ?? `${Math.round(v)}%`);
  return (
    <div
      className="relative inline-flex items-center justify-center"
      style={{ width: size, height: size }}
    >
      <svg
        width={size}
        height={size}
        className={indeterminate ? "animate-spin" : "-rotate-90"}
        style={indeterminate ? { animationDuration: "1.2s" } : undefined}
      >
        <circle
          cx={size / 2}
          cy={size / 2}
          r={r}
          fill="none"
          stroke="currentColor"
          strokeOpacity={0.15}
          strokeWidth={stroke}
        />
        <circle
          cx={size / 2}
          cy={size / 2}
          r={r}
          fill="none"
          stroke="currentColor"
          strokeWidth={stroke}
          strokeLinecap="round"
          strokeDasharray={`${dash} ${c}`}
          className={
            indeterminate
              ? "text-primary"
              : "text-primary transition-[stroke-dasharray] duration-150"
          }
        />
      </svg>
      <span className="absolute text-[10px] font-medium tabular-nums">
        {text}
      </span>
    </div>
  );
}
