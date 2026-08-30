/**
 * App-level preferences, in localStorage.
 *
 * These are machine preferences, not edit state: which Whisper model to use,
 * which language to expect. They deliberately do not live in the project -
 * copying a project to a machine without the model should not break it, and
 * the choice is about this computer's speed/quality tradeoff.
 */

const MODEL_KEY = "wolfcut.transcriber.model";
const LANGUAGE_KEY = "wolfcut.transcriber.language";

/** The default model: the speed/quality sweet spot, per the settings panel. */
export const DEFAULT_TRANSCRIBER_MODEL = "base.en";

export function getTranscriberModel(): string {
  return localStorage.getItem(MODEL_KEY) ?? DEFAULT_TRANSCRIBER_MODEL;
}

export function setTranscriberModel(id: string): void {
  localStorage.setItem(MODEL_KEY, id);
}

/** A Whisper language code, or "auto" to let the model decide. */
export function getTranscriberLanguage(): string {
  return localStorage.getItem(LANGUAGE_KEY) ?? "auto";
}

export function setTranscriberLanguage(code: string): void {
  localStorage.setItem(LANGUAGE_KEY, code);
}

const CONTROL_COLOR_KEY = "wolfcut.controls.color";

/**
 * Only `#rrggbb` is accepted.
 *
 * This value is written straight into a CSS custom property, so it is
 * untrusted input in the one sense that matters: anything else in there is
 * either a rule nobody asked for or a control that renders invisibly. A
 * rejected value reads as "no choice made", which is the theme's own colour.
 */
function validColor(value: string | null): string | null {
  return value !== null && /^#[0-9a-f]{6}$/i.test(value) ? value : null;
}

/** The chosen accent for panel sliders and switches, or null for the theme's. */
export function getControlColor(): string | null {
  return validColor(localStorage.getItem(CONTROL_COLOR_KEY));
}

/**
 * Paints the chosen accent onto the document, or takes it back off.
 *
 * An inline property on the root beats the stylesheet's own, so this needs no
 * cooperation from the theme and survives switching between light and dark.
 * The translucent fill is derived rather than asked for: one colour is one
 * decision, and a fill that did not match its knob could only ever be a
 * mistake.
 */
export function applyControlColor(): void {
  const root = document.documentElement;
  const color = getControlColor();
  if (color === null) {
    root.style.removeProperty("--color-slider");
    root.style.removeProperty("--color-slider-soft");
    return;
  }
  root.style.setProperty("--color-slider", color);
  root.style.setProperty("--color-slider-soft", `color-mix(in srgb, ${color} 18%, transparent)`);
}

export function setControlColor(color: string | null): void {
  if (color === null || validColor(color) === null) localStorage.removeItem(CONTROL_COLOR_KEY);
  else localStorage.setItem(CONTROL_COLOR_KEY, color);
  applyControlColor();
}

/** The accent actually in force, for a colour input to open on. */
export function currentControlColor(): string {
  const chosen = getControlColor();
  if (chosen !== null) return chosen;
  const painted = getComputedStyle(document.documentElement)
    .getPropertyValue("--color-slider")
    .trim();
  return validColor(painted) ?? "#65a30d";
}

const PROXY_DIRECTORY_KEY = "wolfcut.proxy.directory";
const PROXY_HEIGHT_KEY = "wolfcut.proxy.height";
const PROXY_ENABLED_KEY = "wolfcut.proxy.enabled";

/** Proxy sizes offered, mirroring the engine's own list. */
export const PROXY_HEIGHTS = [540, 720, 1080];

/** 720p: more than the monitor draws at the default half-resolution preview. */
export const DEFAULT_PROXY_HEIGHT = 720;

/**
 * Where stand-ins are written. Null until one is chosen.
 *
 * There is deliberately no default folder. Proxies are large and they are the
 * user's files to find, move and delete; putting them somewhere unasked is how
 * an application ends up quietly holding gigabytes that nobody can locate.
 */
export function getProxyDirectory(): string | null {
  return localStorage.getItem(PROXY_DIRECTORY_KEY);
}

export function setProxyDirectory(path: string | null): void {
  if (path === null) localStorage.removeItem(PROXY_DIRECTORY_KEY);
  else localStorage.setItem(PROXY_DIRECTORY_KEY, path);
}

export function getProxyHeight(): number {
  const stored = Number(localStorage.getItem(PROXY_HEIGHT_KEY));
  return PROXY_HEIGHTS.includes(stored) ? stored : DEFAULT_PROXY_HEIGHT;
}

export function setProxyHeight(height: number): void {
  localStorage.setItem(PROXY_HEIGHT_KEY, String(height));
}

/** Whether imports build stand-ins. Off until a folder is chosen. */
export function getProxyEnabled(): boolean {
  return localStorage.getItem(PROXY_ENABLED_KEY) === "true" && getProxyDirectory() !== null;
}

export function setProxyEnabled(on: boolean): void {
  localStorage.setItem(PROXY_ENABLED_KEY, String(on));
}

const TTS_MODEL_KEY = "wolfcut.tts.model";
const TTS_VOICE_KEY = "wolfcut.tts.voice";

/** The default voice model: the compact build the settings panel recommends. */
export const DEFAULT_TTS_MODEL = "kokoro-int8-multi-lang-v1_0";

/** The default speaker: af_heart, Kokoro's showcase voice. */
export const DEFAULT_TTS_VOICE = 3;

export function getTtsModel(): string {
  return localStorage.getItem(TTS_MODEL_KEY) ?? DEFAULT_TTS_MODEL;
}

export function setTtsModel(id: string): void {
  localStorage.setItem(TTS_MODEL_KEY, id);
}

/** A Kokoro speaker id, from the host's voices table. */
export function getTtsVoice(): number {
  const raw = localStorage.getItem(TTS_VOICE_KEY);
  if (raw === null) return DEFAULT_TTS_VOICE;
  const stored = Number(raw);
  return Number.isInteger(stored) && stored >= 0 ? stored : DEFAULT_TTS_VOICE;
}

export function setTtsVoice(id: number): void {
  localStorage.setItem(TTS_VOICE_KEY, String(id));
}
