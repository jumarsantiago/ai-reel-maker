// Editing settings: one flat object the whole UI edits, the built-in presets, how saved settings are
// read back safely, and how they map to the Rust commands. Pure functions only, so they can be unit-tested.

export type Mode = "talk" | "travel";
export type CaptionTemplate = "soft" | "clean" | "blush" | "dark" | "classic";

export type Settings = {
  mode: Mode;
  // Look
  orientation: "vertical" | "horizontal";
  bothShapes: boolean;
  fitBlur: boolean;
  captionTemplate: CaptionTemplate;
  captionFont: string | null;
  // Talking video
  zoom: boolean;
  soundEffects: boolean;
  highlightWords: boolean;
  loudness: boolean;
  reviewCaptions: boolean;
  rememberFixes: boolean;
  burnCaptions: boolean;
  // Travel b-roll
  clipSec: number;
  transition: "smooth" | "dynamic" | "none";
  grade: "warm" | "vivid" | "natural";
  pan: boolean;
  fade: boolean;
  smartPick: boolean;
  beatSync: boolean;
  // Audio (volumes are 0-100 in the UI)
  music: string | null;
  musicVolume: number;
  videoVolume: number;
  muteVideo: boolean;
  duckMusic: boolean;
  musicFade: boolean;
  musicStartSec: number;
  // Reusable text
  cta: string;
  // Where HyperFrames (the free247editor kit) is, for animated graphics; null = look in the usual places.
  kitDir: string | null;
};

export const DEFAULT_SETTINGS: Settings = {
  mode: "talk",
  orientation: "vertical",
  bothShapes: false,
  fitBlur: false,
  captionTemplate: "soft",
  captionFont: null,
  zoom: true,
  soundEffects: true,
  highlightWords: true,
  loudness: true,
  reviewCaptions: true,
  rememberFixes: true,
  burnCaptions: true,
  clipSec: 4,
  transition: "smooth",
  grade: "warm",
  pan: true,
  fade: true,
  smartPick: true,
  beatSync: false,
  music: null,
  musicVolume: 20,
  videoVolume: 100,
  muteVideo: false,
  duckMusic: true,
  musicFade: true,
  musicStartSec: 0,
  cta: "",
  kitDir: null,
};

export type Preset = { name: string; builtIn?: boolean; settings: Partial<Settings> };

export const BUILT_IN_PRESETS: Preset[] = [
  {
    name: "Affiliate reel (TikTok / Reels)",
    builtIn: true,
    settings: {
      mode: "talk", orientation: "vertical", bothShapes: false, fitBlur: false, captionTemplate: "clean",
      zoom: true, soundEffects: true, highlightWords: true, reviewCaptions: true, burnCaptions: true,
      musicVolume: 15, videoVolume: 100, muteVideo: false, duckMusic: true, cta: "CLICK THE YELLOW BASKET",
    },
  },
  {
    name: "YouTube talking video",
    builtIn: true,
    settings: {
      mode: "talk", orientation: "horizontal", bothShapes: false, fitBlur: true, captionTemplate: "dark",
      zoom: true, soundEffects: false, highlightWords: true, reviewCaptions: true, burnCaptions: true,
      musicVolume: 10, videoVolume: 100, muteVideo: false, duckMusic: true,
    },
  },
  {
    name: "Travel reel (beat sync)",
    builtIn: true,
    settings: {
      mode: "travel", orientation: "vertical", bothShapes: false, fitBlur: false, clipSec: 3,
      transition: "smooth", grade: "warm", pan: true, fade: true, smartPick: true, beatSync: true,
      musicVolume: 60, videoVolume: 30, muteVideo: false, duckMusic: false,
    },
  },
  {
    name: "Travel vlog (YouTube)",
    builtIn: true,
    settings: {
      mode: "travel", orientation: "horizontal", bothShapes: false, fitBlur: true, clipSec: 5,
      transition: "smooth", grade: "natural", pan: true, fade: true, smartPick: true, beatSync: false,
      musicVolume: 40, videoVolume: 50, muteVideo: false, duckMusic: false,
    },
  },
  {
    name: "Clean export for CapCut",
    builtIn: true,
    settings: {
      mode: "talk", burnCaptions: false, soundEffects: false, zoom: true, reviewCaptions: true, musicVolume: 0,
    },
  },
];

const clamp = (n: number, lo: number, hi: number) => Math.min(hi, Math.max(lo, n));

/** Saved settings (possibly from an older version, or hand-edited) merged over the defaults, field by field.
 *  Anything of the wrong type is ignored, so a damaged file can never break the app. */
export function restoreSettings(saved: unknown): Settings {
  const out: Settings = { ...DEFAULT_SETTINGS };
  if (!saved || typeof saved !== "object") return out;
  const s = saved as Record<string, unknown>;
  for (const key of Object.keys(DEFAULT_SETTINGS) as (keyof Settings)[]) {
    const def = DEFAULT_SETTINGS[key];
    const v = s[key];
    if (v === undefined) continue;
    if (def === null || key === "music" || key === "captionFont" || key === "kitDir") {
      if (v === null || typeof v === "string") (out as Record<string, unknown>)[key] = v;
    } else if (typeof v === typeof def) {
      (out as Record<string, unknown>)[key] = v;
    }
  }
  const pick = <T extends string>(v: string, allowed: readonly T[], fallback: T): T =>
    (allowed as readonly string[]).includes(v) ? (v as T) : fallback;
  out.mode = pick(out.mode, ["talk", "travel"] as const, "talk");
  out.orientation = pick(out.orientation, ["vertical", "horizontal"] as const, "vertical");
  out.captionTemplate = pick(out.captionTemplate, ["soft", "clean", "blush", "dark", "classic"] as const, "soft");
  out.transition = pick(out.transition, ["smooth", "dynamic", "none"] as const, "smooth");
  out.grade = pick(out.grade, ["warm", "vivid", "natural"] as const, "warm");
  out.musicVolume = clamp(out.musicVolume, 0, 100);
  out.videoVolume = clamp(out.videoVolume, 0, 100);
  out.musicStartSec = clamp(out.musicStartSec, 0, 3600);
  out.clipSec = clamp(out.clipSec, 0, 30);
  return out;
}

/** Applies a preset on top of the current settings (the music file and font stay unless the preset sets them). */
export function applyPreset(current: Settings, preset: Preset): Settings {
  return restoreSettings({ ...current, ...preset.settings });
}

function audio(s: Settings) {
  return {
    musicVolume: s.musicVolume / 100,
    videoVolume: s.videoVolume / 100,
    muteVideo: s.muteVideo,
    duckMusic: s.duckMusic,
    musicFade: s.musicFade,
    musicStartSec: s.musicStartSec,
  };
}

/** The `EditStyle` the Auto Edit commands expect (Rust, camelCase). */
export function toTalkStyle(s: Settings) {
  return {
    zoom: s.zoom,
    soundEffects: s.soundEffects,
    highlightWords: s.highlightWords,
    softCaptions: true,
    loudness: s.loudness,
    music: s.music,
    orientation: s.orientation,
    fitBlur: s.fitBlur,
    captionTemplate: s.captionTemplate,
    captionFont: s.captionFont,
    burnCaptions: s.burnCaptions,
    bothShapes: s.bothShapes,
    ...audio(s),
  };
}

/** The `TravelStyle` the travel commands expect (Rust, camelCase). */
export function toTravelStyle(s: Settings) {
  return {
    clipSec: s.clipSec,
    transition: s.transition,
    pan: s.pan,
    grade: s.grade,
    fade: s.fade,
    music: s.music,
    orientation: s.orientation,
    fit: s.fitBlur ? "blur" : "fill",
    font: s.captionFont,
    bothShapes: s.bothShapes,
    smartPick: s.smartPick,
    beatSync: s.beatSync,
    ...audio(s),
  };
}

export type ExtrasDraft = { hook: string; tags: string };

export type MotionTemplate = "card" | "sticker" | "arrow" | "lowerThird";

/** One animated graphic the user added. `when` is a time in seconds ("12.5") or a phrase said in the video. */
export type MotionDraft = {
  id: number;
  template: MotionTemplate;
  text: string;
  sub: string;
  variant: string;
  position: string;
  accent: string;
  when: string;
  dur: number;
};

export const MOTION_DEFAULTS: Record<MotionTemplate, Partial<MotionDraft>> = {
  card: { variant: "", position: "top", dur: 3 },
  sticker: { variant: "badge", position: "right", dur: 3 },
  arrow: { variant: "down", position: "", dur: 3 },
  lowerThird: { variant: "", position: "bottom", dur: 3.5 },
};

let nextMotionId = 1;
export function newMotion(template: MotionTemplate): MotionDraft {
  // Starts at 1 second so it shows in the first seconds of any preview; change it to place it later.
  return { id: nextMotionId++, template, text: "", sub: "", variant: "", position: "", accent: "rose", when: "1", dur: 3, ...MOTION_DEFAULTS[template] };
}

/** The `MotionSpec`s the commands expect; items with nothing to show are left out. */
export function toMotion(list: MotionDraft[]) {
  return list
    .filter((m) => m.text.trim() || (m.template === "sticker" && m.variant !== "badge") || m.template === "arrow")
    .map((m) => {
      const when = m.when.trim();
      const secs = when !== "" && Number.isFinite(Number(when)) ? Number(when) : null;
      return {
        template: m.template,
        text: m.text.trim(),
        sub: m.sub.trim() || null,
        variant: m.variant || null,
        position: m.position || null,
        accent: m.accent || null,
        at: secs,
        phrase: secs === null && when ? when : null,
        dur: m.dur,
      };
    });
}

/** Hook / tags / CTA / motion graphics for the commands. One tag per line: "TEXT" or "TEXT | phrase said in the video". */
export function toExtras(d: ExtrasDraft, cta: string, motion: MotionDraft[] = [], kitDir: string | null = null) {
  const tags = d.tags
    .split("\n")
    .map((line) => line.trim())
    .filter(Boolean)
    .map((line) => {
      const [text, phrase] = line.split("|").map((x) => x.trim());
      return phrase ? { text, phrase } : { text };
    });
  const hook = d.hook.trim();
  const c = cta.trim();
  const m = toMotion(motion);
  if (!hook && !c && tags.length === 0 && m.length === 0) return null;
  return { hook: hook || null, tags, cta: c || null, motion: m, kitDir };
}

export type SavedPrefs = { last: Settings; presets: Preset[] };

/** Reads the prefs file's contents (any shape) into settings + the user's own presets. */
export function restorePrefs(raw: unknown): SavedPrefs {
  const r = raw && typeof raw === "object" ? (raw as Record<string, unknown>) : {};
  const presets = Array.isArray(r.presets)
    ? r.presets
        .filter((p): p is { name: string; settings: object } => !!p && typeof p === "object" && typeof (p as Preset).name === "string")
        .map((p) => ({ name: p.name.slice(0, 60), settings: restoreSettings(p.settings) }))
    : [];
  return { last: restoreSettings(r.last), presets };
}
