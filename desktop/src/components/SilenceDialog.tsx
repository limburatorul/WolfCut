import { useEffect, useState } from "react";

import { useLocale } from "../lib/i18n";
import { Slider, Toggle } from "./controls";
import { Icon } from "./Icon";

/** Quietest level still counted as sound. Mirrors the engine's default. */
const DEFAULT_THRESHOLD_DB = -50;
/** Shortest pause worth cutting, in seconds. Mirrors the engine's default. */
const DEFAULT_MIN_DURATION = 0.5;

/**
 * The two numbers that decide which pauses get cut.
 *
 * They are asked for rather than guessed because they describe the recording,
 * not the app: a noise-reduced voiceover and a handheld take in a cafe
 * disagree about what silence sounds like by twenty decibels or more. The
 * defaults suit a clean voice track, which is what this mostly runs on, and
 * the sheet stays open on a miss so the next attempt is one drag away rather
 * than a whole trip back through the menu.
 */
export function SilenceDialog({
  clipName,
  busy,
  onRun,
  onCancel,
}: {
  /** The clip being cut, named so the sheet says what it is about to change. */
  clipName: string;
  /** True while the detector is running; the run button reports it. */
  busy: boolean;
  onRun: (thresholdDb: number, minDuration: number, ripple: boolean) => void;
  onCancel: () => void;
}) {
  const [thresholdDb, setThresholdDb] = useState(DEFAULT_THRESHOLD_DB);
  const [minDuration, setMinDuration] = useState(DEFAULT_MIN_DURATION);
  const [ripple, setRipple] = useState(true);
  const { t } = useLocale();

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape" && !busy) onCancel();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onCancel, busy]);

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/40 p-8
                 backdrop-blur-[2px]"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget && !busy) onCancel();
      }}
    >
      <div className="surface w-full max-w-sm rounded-2xl p-6">
        <div className="mb-2 flex items-center gap-2">
          <Icon name="waveform" size={16} className="text-accent" />
          <h2 className="text-sm font-semibold text-primary">{t("silence.title")}</h2>
        </div>
        <p className="mb-5 text-xs leading-relaxed text-secondary">
          {t("silence.description", { name: clipName })}
        </p>

        <Slider
          label={t("silence.threshold")}
          value={thresholdDb}
          min={-80}
          max={-20}
          step={1}
          format={(value) => `${Math.round(value)} dB`}
          onChange={setThresholdDb}
          onReset={() => setThresholdDb(DEFAULT_THRESHOLD_DB)}
        />
        <Slider
          label={t("silence.minGap")}
          value={minDuration}
          min={0.1}
          max={3}
          step={0.05}
          format={(value) => `${value.toFixed(2)} s`}
          onChange={setMinDuration}
          onReset={() => setMinDuration(DEFAULT_MIN_DURATION)}
        />
        <Toggle
          label={t("silence.ripple")}
          hint={t("silence.rippleHint")}
          checked={ripple}
          onChange={setRipple}
        />

        <div className="mt-5 flex gap-1.5">
          <button
            type="button"
            disabled={busy}
            onClick={onCancel}
            className="flex-1 cursor-pointer rounded-lg bg-hover px-4 py-2 text-sm text-primary
                       transition-colors hover:bg-active disabled:cursor-not-allowed
                       disabled:opacity-40"
          >
            {t("common.cancel")}
          </button>
          <button
            type="button"
            disabled={busy}
            onClick={() => onRun(thresholdDb, minDuration, ripple)}
            className="flex-1 cursor-pointer rounded-lg bg-accent px-4 py-2 text-sm font-medium
                       text-on-accent transition-colors hover:bg-accent-hover
                       disabled:cursor-not-allowed disabled:opacity-40"
          >
            {busy ? t("silence.working") : t("silence.run")}
          </button>
        </div>
      </div>
    </div>
  );
}
