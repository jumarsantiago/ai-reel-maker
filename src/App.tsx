import { useEffect, useRef, useState } from "react";
import { invoke, convertFileSrc } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open, save } from "@tauri-apps/plugin-dialog";
import {
  TimelineClip, createClip, setTrim, moveClip, removeClip, clipLength, totalDuration, toRenderPlan,
} from "./timeline.ts";
import {
  BUILT_IN_PRESETS, DEFAULT_SETTINGS, ExtrasDraft, MotionDraft, Preset, Settings, applyPreset, restorePrefs, toExtras, toMotion, toTalkStyle,
  toTravelStyle,
} from "./settings.ts";
import { FontInfo, Inspector, KitStatus } from "./Inspector.tsx";
import {
  AutoCutResult, BatchDialog, BatchItem, ErrorDialog, ExportDialog, ExportResult, ExportState, PreviewDialog, ProgressDialog, ResultDialog,
  ReviewDialog, ReviewState, SettingsDialog, formatSize, formatTime, plural, reviewEdits,
} from "./dialogs.tsx";
import { Modal } from "./ui.tsx";
import "./App.css";

type MediaInfo = {
  path: string;
  fileName: string;
  sizeBytes: number;
  durationSec: number;
  width: number;
  height: number;
  videoCodec: string;
  fps: number;
  hasAudio: boolean;
};

type CaptionDraft = { index: number; text: string; start: number; end: number };
type PrepareResult = { projectDir: string; sourceDurationSec: number; captions: CaptionDraft[] };
type PreviewResult = { outputPath: string; durationSec: number };

type Job =
  | { kind: "idle" }
  | { kind: "running"; title: string; source: string; stage: string; progress: number }
  | { kind: "done"; result: AutoCutResult }
  | { kind: "error"; message: string };

type Selection = { kind: "media"; path: string } | { kind: "clip"; id: string } | null;

const VIDEO_EXTENSIONS = ["mp4", "mov", "m4v", "webm", "mkv"];
const MUSIC_EXTENSIONS = ["mp3", "m4a", "aac", "wav", "aif", "aiff", "flac"];

function App() {
  const [media, setMedia] = useState<MediaInfo[]>([]);
  const [clips, setClips] = useState<TimelineClip[]>([]);
  const [selection, setSelection] = useState<Selection>(null);
  const [errors, setErrors] = useState<string[]>([]);
  const [importing, setImporting] = useState(false);
  const [trimDraft, setTrimDraft] = useState({ in: "", out: "" });
  const [trimError, setTrimError] = useState<string | null>(null);
  const [exportState, setExportState] = useState<ExportState>({ kind: "idle" });

  const [settings, setSettings] = useState<Settings>(DEFAULT_SETTINGS);
  const [userPresets, setUserPresets] = useState<Preset[]>([]);
  const [prefsLoaded, setPrefsLoaded] = useState(false);
  const [presetName, setPresetName] = useState<string | null>(null);
  const [extras, setExtras] = useState<ExtrasDraft>({ hook: "", tags: "" });
  const [motion, setMotion] = useState<MotionDraft[]>([]);
  const [kit, setKit] = useState<KitStatus | null>(null);
  const [fonts, setFonts] = useState<FontInfo[]>([]);
  const [fontPreview, setFontPreview] = useState<string | null>(null);

  const [job, setJob] = useState<Job>({ kind: "idle" });
  const [review, setReview] = useState<ReviewState | null>(null);
  const [previewPath, setPreviewPath] = useState<string | null>(null);
  const [previewTitle, setPreviewTitle] = useState<string | undefined>(undefined);
  const [batch, setBatch] = useState<{ items: BatchItem[]; running: boolean } | null>(null);
  const [batchSel, setBatchSel] = useState<string[]>([]);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const videoRef = useRef<HTMLVideoElement>(null);

  const selectedMedia = selection?.kind === "media" ? media.find((m) => m.path === selection.path) ?? null : null;
  const clipIndex = selection?.kind === "clip" ? clips.findIndex((c) => c.id === selection.id) : -1;
  const selectedClip = clipIndex >= 0 ? clips[clipIndex] : null;
  const playerPath = selectedClip?.sourcePath ?? selectedMedia?.path ?? null;
  const playerInfo = media.find((m) => m.path === playerPath) ?? null;
  const plan = toRenderPlan(clips);
  const update = (patch: Partial<Settings>) => setSettings((s) => ({ ...s, ...patch }));

  // Saved settings and presets.
  useEffect(() => {
    invoke<unknown>("get_prefs")
      .then((raw) => {
        const p = restorePrefs(raw);
        setSettings(p.last);
        setUserPresets(p.presets);
      })
      .catch(() => {})
      .finally(() => setPrefsLoaded(true));
    invoke<FontInfo[]>("list_fonts").then(setFonts).catch(() => setFonts([]));
  }, []);

  useEffect(() => {
    if (!prefsLoaded) return;
    const t = setTimeout(() => {
      invoke("set_prefs", { value: { last: settings, presets: userPresets } }).catch(() => {});
    }, 400);
    return () => clearTimeout(t);
  }, [settings, userPresets, prefsLoaded]);

  // Is HyperFrames available? Checked when a graphic is added or the folder changes.
  useEffect(() => {
    if (!prefsLoaded || motion.length === 0) return;
    invoke<KitStatus>("kit_status", { dir: settings.kitDir }).then(setKit).catch(() => setKit(null));
  }, [prefsLoaded, motion.length > 0, settings.kitDir, settingsOpen]);

  useEffect(() => {
    let cancelled = false;
    invoke<string>("caption_preview", {
      font: settings.captionFont,
      text: settings.mode === "travel" ? "Island hopping sa Siargao!" : null,
      template: settings.mode === "travel" ? "soft" : settings.captionTemplate,
    })
      .then((url) => !cancelled && setFontPreview(url))
      .catch(() => !cancelled && setFontPreview(null));
    return () => {
      cancelled = true;
    };
  }, [settings.captionFont, settings.captionTemplate, settings.mode]);

  useEffect(() => {
    if (selectedClip) setTrimDraft({ in: selectedClip.inPoint.toFixed(2), out: selectedClip.outPoint.toFixed(2) });
    setTrimError(null);
  }, [selectedClip?.id, selectedClip?.inPoint, selectedClip?.outPoint]);

  async function importMedia() {
    const picked = await open({ multiple: true, directory: false, filters: [{ name: "Video", extensions: VIDEO_EXTENSIONS }] });
    if (!picked) return;
    const paths = Array.isArray(picked) ? picked : [picked];
    setImporting(true);
    const newErrors: string[] = [];
    const added: MediaInfo[] = [];
    for (const path of paths) {
      if (media.some((m) => m.path === path)) continue;
      try {
        added.push(await invoke<MediaInfo>("probe_media", { path }));
      } catch (e) {
        newErrors.push(`${path.split("/").pop()}: ${String(e)}`);
      }
    }
    setMedia((prev) => [...prev, ...added]);
    if (added.length > 0) setSelection({ kind: "media", path: added[0].path });
    setErrors(newErrors);
    setImporting(false);
  }

  function addToTimeline(m: MediaInfo) {
    const clip = createClip(m);
    setClips((prev) => [...prev, clip]);
    setSelection({ kind: "clip", id: clip.id });
  }

  /** Takes a video out of the app (and its clips off the timeline). The file on disk is not touched. */
  function removeMedia(path: string) {
    setMedia((prev) => prev.filter((m) => m.path !== path));
    setClips((prev) => prev.filter((c) => c.sourcePath !== path));
    setBatchSel((prev) => prev.filter((p) => p !== path));
    setSelection((sel) => {
      if (sel?.kind === "media" && sel.path === path) return null;
      if (sel?.kind === "clip" && clips.find((c) => c.id === sel.id)?.sourcePath === path) return null;
      return sel;
    });
  }

  function addAllToTimeline() {
    const added = media.map((m) => createClip(m));
    setClips((prev) => [...prev, ...added]);
    if (added.length) setSelection({ kind: "clip", id: added[0].id });
  }

  function applyTrim(inPoint: number, outPoint: number) {
    if (!selectedClip) return;
    const r = setTrim(selectedClip, inPoint, outPoint);
    if (r.ok) {
      setClips((prev) => prev.map((c) => (c.id === r.clip.id ? r.clip : c)));
      setTrimError(null);
    } else {
      setTrimError(r.error);
    }
  }

  const playhead = () => Math.round((videoRef.current?.currentTime ?? 0) * 100) / 100;

  // When playing a timeline clip, keep playback inside its in/out range.
  function onLoadedMetadata() {
    if (selectedClip && videoRef.current) videoRef.current.currentTime = selectedClip.inPoint;
  }
  function onTimeUpdate() {
    const v = videoRef.current;
    if (selectedClip && v && v.currentTime >= selectedClip.outPoint) {
      v.pause();
      v.currentTime = selectedClip.outPoint;
    }
  }
  function onPlay() {
    const v = videoRef.current;
    if (selectedClip && v && (v.currentTime < selectedClip.inPoint || v.currentTime >= selectedClip.outPoint - 0.05)) {
      v.currentTime = selectedClip.inPoint;
    }
  }

  async function exportTimeline() {
    if (!plan.ok) return;
    const picked = await save({ title: "Export MP4", defaultPath: "timeline.mp4", filters: [{ name: "MP4 video", extensions: ["mp4"] }] });
    if (!picked) return;
    const needsExt = !picked.toLowerCase().endsWith(".mp4");
    const outputPath = needsExt ? `${picked}.mp4` : picked;
    setExportState({ kind: "rendering", outputPath, progress: 0 });
    const unlisten = await listen<number>("export-progress", (e) =>
      setExportState((s) => (s.kind === "rendering" ? { ...s, progress: e.payload } : s)),
    );
    try {
      const result = await invoke<ExportResult>("export_video", {
        request: {
          segments: plan.segments.map(({ sourcePath, inPoint, outPoint }) => ({ sourcePath, inPoint, outPoint })),
          outputPath,
          // The native save panel asks before replacing; a name we changed was never confirmed.
          overwriteConfirmed: !needsExt,
        },
      });
      setExportState({ kind: "done", result });
    } catch (e) {
      setExportState({ kind: "error", message: String(e) });
    } finally {
      unlisten();
    }
  }

  const timelineSegments = () =>
    plan.ok ? plan.segments.map((sg) => ({ sourcePath: sg.sourcePath, inPoint: sg.inPoint, outPoint: sg.outPoint })) : null;
  const extrasArg = () => toExtras(extras, settings.cta, motion, settings.kitDir, settings);

  /** Runs `work` while the progress dialog shows the stage text and render progress. */
  async function withProgress<T>(title: string, source: string, work: () => Promise<T>): Promise<T | undefined> {
    setJob({ kind: "running", title, source, stage: "Starting…", progress: 0 });
    const offStage = await listen<string>("autocut-stage", (e) => setJob((j) => (j.kind === "running" ? { ...j, stage: e.payload } : j)));
    const offProgress = await listen<number>("autocut-progress", (e) => setJob((j) => (j.kind === "running" ? { ...j, progress: e.payload } : j)));
    const offPreview = await listen<number>("preview-progress", (e) => setJob((j) => (j.kind === "running" ? { ...j, stage: "Rendering preview…", progress: e.payload } : j)));
    try {
      return await work();
    } catch (e) {
      setJob({ kind: "error", message: String(e) });
      return undefined;
    } finally {
      offStage();
      offProgress();
      offPreview();
    }
  }

  // What the main button works on, and why it cannot run yet.
  const timelineHasSound = clips.some((c) => media.find((m) => m.path === c.sourcePath)?.hasAudio);
  let target: string | null = null;
  let blocker: string | null = null;
  if (settings.mode === "travel") {
    if (clips.length === 0) blocker = "Add your clips to the timeline with + first.";
    else target = plural(clips.length, "timeline clip");
  } else if (clips.length > 0) {
    if (!timelineHasSound) blocker = "The timeline clips have no sound to transcribe.";
    else target = plural(clips.length, "timeline clip");
  } else if (selectedMedia) {
    if (!selectedMedia.hasAudio) blocker = "This video has no sound to transcribe.";
    else target = selectedMedia.fileName;
  } else {
    blocker = media.length ? "Select a video, or add clips to the timeline." : "Import a video to begin.";
  }
  if (!blocker && !plan.ok && clips.length > 0) blocker = plan.error;

  async function run() {
    if (blocker) return;
    if (settings.mode === "travel") {
      const segments = timelineSegments();
      if (!segments) return;
      const result = await withProgress("Making the travel reel", target ?? "", () =>
        invoke<AutoCutResult>("travel_edit", { segments, style: toTravelStyle(settings), extras: extrasArg() }),
      );
      if (result) setJob({ kind: "done", result });
      return;
    }
    const style = toTalkStyle(settings);
    const segments = clips.length > 0 ? timelineSegments() : null;
    const source = segments ? { kind: "timeline", segments } : { kind: "video", path: selectedMedia!.path };
    if (settings.reviewCaptions) {
      const prepared = await withProgress("Auto Edit", target ?? "", () =>
        invoke<PrepareResult>("auto_cut_prepare", { source, style, extras: extrasArg() }),
      );
      if (!prepared) return;
      setReview({
        dir: prepared.projectDir,
        sourceDurationSec: prepared.sourceDurationSec,
        label: target ?? "",
        captions: prepared.captions.map((c) => ({ ...c, original: c.text, deleted: false })),
      });
      setJob({ kind: "idle" });
      return;
    }
    const result = await withProgress("Auto Edit", target ?? "", () =>
      segments
        ? invoke<AutoCutResult>("auto_cut_timeline", { segments, style, extras: extrasArg() })
        : invoke<AutoCutResult>("auto_cut", { path: selectedMedia!.path, style, extras: extrasArg() }),
    );
    if (result) setJob({ kind: "done", result });
  }

  async function renderReviewed() {
    if (!review) return;
    const r = review;
    setReview(null);
    const result = await withProgress("Auto Edit", r.label, () =>
      invoke<AutoCutResult>("auto_cut_render", {
        projectDir: r.dir,
        sourceDurationSec: r.sourceDurationSec,
        style: toTalkStyle(settings),
        extras: extrasArg(),
        edits: reviewEdits(r),
        rememberFixes: settings.rememberFixes,
      }),
    );
    if (result) setJob({ kind: "done", result });
  }

  async function previewReviewed(): Promise<string> {
    if (!review) throw new Error("Nothing to preview.");
    const r = await invoke<PreviewResult>("auto_cut_preview", {
      projectDir: review.dir,
      style: toTalkStyle(settings),
      extras: extrasArg(),
      edits: reviewEdits(review),
    });
    return r.outputPath;
  }

  async function previewTravel() {
    const segments = timelineSegments();
    if (!segments || blocker) return;
    const r = await withProgress("Preview", "First 10 seconds", () =>
      invoke<PreviewResult>("travel_preview", { segments, style: toTravelStyle(settings), extras: extrasArg() }),
    );
    if (r) {
      setJob({ kind: "idle" });
      setPreviewTitle(undefined);
      setPreviewPath(r.outputPath);
    }
  }

  /** Plays one graphic over the selected video's first seconds (or a plain backdrop). */
  async function previewMotion(m: MotionDraft) {
    const [spec] = toMotion([m]);
    if (!spec) throw new Error("Add some text first (a check mark, star or arrow needs none).");
    const background = (selectedClip ? selectedClip.sourcePath : selectedMedia?.path) ?? null;
    const path = await invoke<string>("motion_preview", {
      spec,
      orientation: settings.orientation,
      kitDir: settings.kitDir,
      background,
    });
    setPreviewTitle("Graphic preview");
    setPreviewPath(path);
  }

  async function runBatch() {
    const items: BatchItem[] = media
      .filter((m) => batchSel.includes(m.path) && m.hasAudio)
      .map((m) => ({ path: m.path, name: m.fileName, status: "waiting" }));
    if (!items.length) return;
    setBatch({ items, running: true });
    const style = { ...toTalkStyle(settings) };
    const setItem = (path: string, patch: Partial<BatchItem>) =>
      setBatch((b) => b && { ...b, items: b.items.map((it) => (it.path === path ? { ...it, ...patch } : it)) });
    let current = "";
    const offStage = await listen<string>("autocut-stage", (e) => current && setItem(current, { stage: e.payload }));
    try {
      for (const it of items) {
        current = it.path;
        setItem(it.path, { status: "running", stage: "Starting…" });
        try {
          const r = await invoke<AutoCutResult>("auto_cut", { path: it.path, style, extras: extrasArg() });
          setItem(it.path, { status: "done", output: r.outputPath });
        } catch (e) {
          setItem(it.path, { status: "error", error: String(e) });
        }
      }
    } finally {
      offStage();
      setBatch((b) => b && { ...b, running: false });
    }
  }

  async function chooseMusic() {
    const picked = await open({ multiple: false, directory: false, filters: [{ name: "Audio", extensions: MUSIC_EXTENSIONS }] });
    if (typeof picked === "string") update({ music: picked });
  }

  async function openInPreview(path: string) {
    try {
      const info = await invoke<MediaInfo>("probe_media", { path });
      setMedia((prev) => (prev.some((x) => x.path === info.path) ? prev : [...prev, info]));
      setSelection({ kind: "media", path: info.path });
      setJob({ kind: "idle" });
    } catch (e) {
      setJob({ kind: "error", message: String(e) });
    }
  }

  // Presets
  const allPresets = [...BUILT_IN_PRESETS, ...userPresets];
  function choosePreset(name: string) {
    const p = allPresets.find((x) => x.name === name);
    if (p) setSettings((s) => applyPreset(s, p));
  }
  function savePreset(name: string) {
    const clean = name.trim().slice(0, 60);
    if (!clean) return;
    setUserPresets((prev) => [...prev.filter((p) => p.name !== clean), { name: clean, settings }]);
    setPresetName(null);
  }

  const total = totalDuration(clips);
  const busy = job.kind === "running" || !!batch?.running;
  const talkBatchCount = media.filter((m) => batchSel.includes(m.path) && m.hasAudio).length;

  return (
    <div className="app">
      <header className="topbar">
        <div className="brand">
          <span className="logo" aria-hidden="true" />
          <span>AI Reel Maker</span>
        </div>
        <div className="topbar-center">
          <label className="preset-picker">
            <span className="muted">Preset</span>
            <select value="" onChange={(e) => e.target.value && choosePreset(e.target.value)} aria-label="Apply a preset">
              <option value="">Apply a preset…</option>
              <optgroup label="Ready-made">
                {BUILT_IN_PRESETS.map((p) => <option key={p.name} value={p.name}>{p.name}</option>)}
              </optgroup>
              {userPresets.length > 0 && (
                <optgroup label="My presets">
                  {userPresets.map((p) => <option key={p.name} value={p.name}>{p.name}</option>)}
                </optgroup>
              )}
            </select>
          </label>
          <button className="btn small" onClick={() => setPresetName("")}>Save as preset</button>
        </div>
        <div className="topbar-actions">
          <button className="btn small" onClick={() => setSettingsOpen(true)}>Settings</button>
          <button className="btn small" onClick={exportTimeline} disabled={!plan.ok || exportState.kind === "rendering"} title={plan.ok ? "Join the timeline clips into one MP4, without editing" : plan.error}>
            Export timeline
          </button>
        </div>
      </header>

      <aside className="library" aria-label="Media">
        <div className="panel-head">
          <h2>Media</h2>
          <button className="btn small primary" onClick={importMedia} disabled={importing}>{importing ? "Importing…" : "+ Import"}</button>
        </div>
        <div className="library-list">
          {media.length === 0 && (
            <div className="empty-hint">
              <p>Your videos will show up here.</p>
              <button className="btn" onClick={importMedia}>Import videos</button>
            </div>
          )}
          {media.map((m) => {
            const active = selection?.kind === "media" && selection.path === m.path;
            const checked = batchSel.includes(m.path);
            return (
              <div key={m.path} className={`media-card${active ? " active" : ""}`}>
                <input
                  type="checkbox"
                  className="media-check"
                  aria-label={`Select ${m.fileName} for batch editing`}
                  checked={checked}
                  onChange={(e) => setBatchSel((s) => (e.target.checked ? [...s, m.path] : s.filter((p) => p !== m.path)))}
                />
                <button className="media-main" onClick={() => setSelection({ kind: "media", path: m.path })} title={m.path}>
                  <span className="media-name">{m.fileName}</span>
                  <span className="media-meta">
                    {formatTime(m.durationSec)} · {m.width}×{m.height}
                    {!m.hasAudio && <span className="chip">no sound</span>}
                  </span>
                </button>
                <button className="icon-btn add" onClick={() => addToTimeline(m)} title="Add to timeline" aria-label={`Add ${m.fileName} to the timeline`}>+</button>
                <button
                  className="icon-btn remove"
                  onClick={() => removeMedia(m.path)}
                  title="Remove from the app (the file stays on your Mac)"
                  aria-label={`Remove ${m.fileName} from the media list`}
                >
                  ✕
                </button>
              </div>
            );
          })}
        </div>
        {media.length > 1 && (
          <div className="library-foot">
            <button className="btn small block" onClick={addAllToTimeline}>Add all to timeline</button>
            {settings.mode === "talk" && (
              <button className="btn small block" onClick={runBatch} disabled={talkBatchCount === 0 || busy} title="Edit each checked video on its own, one after another">
                Auto Edit {talkBatchCount || ""} checked {talkBatchCount === 1 ? "video" : "videos"}
              </button>
            )}
          </div>
        )}
        {errors.length > 0 && (
          <div className="error-text" role="alert">
            {errors.map((e) => <p key={e}>{e}</p>)}
          </div>
        )}
      </aside>

      <main className="stage">
        <div className="player">
          {playerPath ? (
            <video
              key={`${playerPath}|${selectedClip?.id ?? "media"}`}
              ref={videoRef}
              src={convertFileSrc(playerPath)}
              controls
              onLoadedMetadata={onLoadedMetadata}
              onTimeUpdate={onTimeUpdate}
              onPlay={onPlay}
            />
          ) : (
            <div className="welcome">
              <h1>Make a reel in three steps</h1>
              <ol>
                <li><strong>Import</strong> your videos.</li>
                <li>Press <strong>+</strong> to put them on the timeline (or just select one).</li>
                <li>Choose <strong>Talking video</strong> or <strong>Travel b-roll</strong> on the right, then press the big button.</li>
              </ol>
              <button className="btn primary" onClick={importMedia}>Import videos</button>
            </div>
          )}
        </div>
        {playerInfo && (
          <div className="player-info">
            <span className="strong ellipsis">{playerInfo.fileName}</span>
            <span className="muted">
              {formatTime(playerInfo.durationSec)} · {playerInfo.width}×{playerInfo.height} · {playerInfo.fps.toFixed(0)} fps ·{" "}
              {playerInfo.hasAudio ? "with sound" : "no sound"} · {formatSize(playerInfo.sizeBytes)}
            </span>
          </div>
        )}
        {selectedClip && (
          <div className="clip-bar" aria-label="Selected clip">
            <span className="clip-badge">Clip {clipIndex + 1}</span>
            <form
              className="trim"
              onSubmit={(e) => {
                e.preventDefault();
                applyTrim(parseFloat(trimDraft.in), parseFloat(trimDraft.out));
              }}
            >
              <label>In <input type="number" step="0.01" min="0" value={trimDraft.in} onChange={(e) => setTrimDraft((d) => ({ ...d, in: e.target.value }))} /></label>
              <label>Out <input type="number" step="0.01" min="0" value={trimDraft.out} onChange={(e) => setTrimDraft((d) => ({ ...d, out: e.target.value }))} /></label>
              <button className="btn small" type="submit">Apply</button>
            </form>
            <button className="btn small" onClick={() => applyTrim(playhead(), selectedClip.outPoint)} title="Start the clip where the player is">⇤ In</button>
            <button className="btn small" onClick={() => applyTrim(selectedClip.inPoint, playhead())} title="End the clip where the player is">Out ⇥</button>
            <span className="grow" />
            <button className="icon-btn" onClick={() => setClips((p) => moveClip(p, clipIndex, -1))} disabled={clipIndex === 0} aria-label="Move earlier">◀</button>
            <button className="icon-btn" onClick={() => setClips((p) => moveClip(p, clipIndex, 1))} disabled={clipIndex === clips.length - 1} aria-label="Move later">▶</button>
            <button className="btn small danger" onClick={() => { setClips((p) => removeClip(p, selectedClip.id)); setSelection(null); }}>Remove</button>
            {trimError && <span className="error-text">{trimError}</span>}
          </div>
        )}
      </main>

      <Inspector
        settings={settings}
        update={update}
        fonts={fonts}
        fontPreview={fontPreview}
        extras={extras}
        setExtras={setExtras}
        chooseMusic={chooseMusic}
        motion={motion}
        setMotion={setMotion}
        kit={kit}
        openSettings={() => setSettingsOpen(true)}
        previewMotion={previewMotion}
        previewOver={selectedClip ? selectedClip.fileName : selectedMedia?.fileName ?? null}
        target={target}
        blocker={blocker}
        running={busy}
        onRun={run}
        onPreview={previewTravel}
      />

      <footer className="timeline" aria-label="Timeline">
        <div className="panel-head">
          <h2>Timeline</h2>
          <span className="muted">{plural(clips.length, "clip")} · {formatTime(total)}</span>
          {clips.length > 0 && (
            <button className="link-btn" onClick={() => { setClips([]); setSelection(null); }}>Clear</button>
          )}
        </div>
        {clips.length === 0 ? (
          <p className="muted timeline-empty">Press + on a video to add it here. Clips play in this order.</p>
        ) : (
          <div className="track">
            {clips.map((c, i) => (
              <button
                key={c.id}
                className={`track-clip${c.id === selectedClip?.id ? " active" : ""}`}
                style={{ flexGrow: clipLength(c) }}
                onClick={() => setSelection({ kind: "clip", id: c.id })}
                title={`${c.fileName} ${c.inPoint.toFixed(2)}–${c.outPoint.toFixed(2)}s`}
              >
                <span className="track-num">{i + 1}</span>
                <span className="ellipsis">{c.fileName}</span>
                <span className="muted">{clipLength(c).toFixed(1)}s</span>
              </button>
            ))}
          </div>
        )}
      </footer>

      {job.kind === "running" && <ProgressDialog title={job.title} source={job.source} stage={job.stage} progress={job.progress} />}
      {job.kind === "error" && <ErrorDialog message={job.message} onClose={() => setJob({ kind: "idle" })} />}
      {job.kind === "done" && (
        <ResultDialog
          result={job.result}
          thumbnailText={extras.hook.trim() || settings.cta.trim()}
          thumbnailStyle={toTalkStyle(settings)}
          font={settings.captionFont}
          onAddToMedia={openInPreview}
          onClose={() => setJob({ kind: "idle" })}
        />
      )}
      {review && job.kind === "idle" && (
        <ReviewDialog review={review} setReview={setReview} onRender={renderReviewed} onCancel={() => setReview(null)} onPreview={previewReviewed} />
      )}
      {previewPath && <PreviewDialog path={previewPath} title={previewTitle} onClose={() => setPreviewPath(null)} />}
      {batch && <BatchDialog items={batch.items} running={batch.running} onClose={() => setBatch(null)} />}
      {settingsOpen && <SettingsDialog kitDir={settings.kitDir} onKitDir={(kitDir) => update({ kitDir })} onClose={() => setSettingsOpen(false)} />}
      <ExportDialog state={exportState} onClose={() => setExportState({ kind: "idle" })} />
      {presetName !== null && (
        <Modal
          title="Save as preset"
          onClose={() => setPresetName(null)}
          footer={
            <>
              <button className="btn" onClick={() => setPresetName(null)}>Cancel</button>
              <button className="btn primary" onClick={() => savePreset(presetName)} disabled={!presetName.trim()}>Save</button>
            </>
          }
        >
          <form onSubmit={(e) => { e.preventDefault(); savePreset(presetName); }}>
            <div className="field">
              <label htmlFor="preset-name">Name</label>
              <input id="preset-name" autoFocus value={presetName} placeholder="e.g. My affiliate style" onChange={(e) => setPresetName(e.target.value)} />
            </div>
          </form>
          <p className="note">Saves every setting on the right (look, audio, call to action). A preset with the same name is replaced.</p>
          {userPresets.length > 0 && (
            <div className="preset-list">
              <span className="muted">My presets</span>
              {userPresets.map((p) => (
                <div key={p.name} className="preset-row">
                  <span className="ellipsis">{p.name}</span>
                  <button className="link-btn" onClick={() => setUserPresets((prev) => prev.filter((x) => x.name !== p.name))}>Delete</button>
                </div>
              ))}
            </div>
          )}
        </Modal>
      )}
    </div>
  );
}

export default App;
