import { useEffect, useRef, useState } from "react";
import { invoke, convertFileSrc } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open, save } from "@tauri-apps/plugin-dialog";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import {
  TimelineClip, createClip, setTrim, moveClip, removeClip, clipLength, totalDuration, toRenderPlan,
} from "./timeline.ts";
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

type ExportResult = {
  outputPath: string;
  durationSec: number;
  width: number;
  height: number;
  hasAudio: boolean;
  sizeBytes: number;
};

type ExportState =
  | { kind: "idle" }
  | { kind: "rendering"; outputPath: string; progress: number }
  | { kind: "done"; result: ExportResult }
  | { kind: "error"; message: string };

type AutoCutResult = {
  mode: "talk" | "travel";
  projectDir: string;
  outputPath: string;
  durationSec: number;
  sourceDurationSec: number;
  segments: number;
  captions: number;
  pausesRemoved: number;
  fillersRemoved: number;
  repeatsRemoved: number;
  keptWordsMatch: number;
};

type EditStyle = {
  zoom: boolean;
  soundEffects: boolean;
  highlightWords: boolean;
  loudness: boolean;
  music: string | null;
};

type TravelDraft = {
  clipSec: number;
  transition: "smooth" | "dynamic" | "none";
  grade: "warm" | "vivid" | "natural";
  pan: boolean;
  fade: boolean;
};

type ExtrasDraft = { hook: string; tags: string; cta: string };

// One tag per line: "TEXT" or "TEXT | phrase said in the video" (the tag appears when that phrase is spoken).
function toExtras(d: ExtrasDraft) {
  const tags = d.tags
    .split("\n")
    .map((line) => line.trim())
    .filter(Boolean)
    .map((line) => {
      const [text, phrase] = line.split("|").map((x) => x.trim());
      return phrase ? { text, phrase } : { text };
    });
  const hook = d.hook.trim();
  const cta = d.cta.trim();
  if (!hook && !cta && tags.length === 0) return null;
  return { hook: hook || null, tags, cta: cta || null };
}

const MUSIC_EXTENSIONS = ["mp3", "m4a", "aac", "wav", "aif", "aiff", "flac"];

type AutoCutState =
  | { kind: "idle" }
  | { kind: "running"; source: string; stage: string; progress: number }
  | { kind: "done"; result: AutoCutResult }
  | { kind: "error"; message: string };

type KeyStatus = { source: "env" | "keychain" | "none"; account: string };

type Selection = { kind: "media"; path: string } | { kind: "clip"; id: string } | null;

const VIDEO_EXTENSIONS = ["mp4", "mov", "m4v", "webm", "mkv"];

function formatTime(sec: number) {
  const m = Math.floor(sec / 60);
  const s = (sec % 60).toFixed(2).padStart(5, "0");
  return `${m}:${s}`;
}

function plural(n: number, word: string) {
  return `${n} ${word}${n === 1 ? "" : "s"}`;
}

function formatSize(bytes: number) {
  return bytes >= 1e9 ? `${(bytes / 1e9).toFixed(2)} GB` : `${(bytes / 1e6).toFixed(1)} MB`;
}

function App() {
  const [media, setMedia] = useState<MediaInfo[]>([]);
  const [clips, setClips] = useState<TimelineClip[]>([]);
  const [selection, setSelection] = useState<Selection>(null);
  const [errors, setErrors] = useState<string[]>([]);
  const [busy, setBusy] = useState(false);
  const [trimDraft, setTrimDraft] = useState({ in: "", out: "" });
  const [trimError, setTrimError] = useState<string | null>(null);
  const [exportState, setExportState] = useState<ExportState>({ kind: "idle" });
  const [autoCut, setAutoCut] = useState<AutoCutState>({ kind: "idle" });
  const [editStyle, setEditStyle] = useState<EditStyle>({
    zoom: true,
    soundEffects: true,
    highlightWords: true,
    loudness: true,
    music: null,
  });
  const [editMode, setEditMode] = useState<"talk" | "travel">("talk");
  const [travelDraft, setTravelDraft] = useState<TravelDraft>({ clipSec: 4, transition: "smooth", grade: "warm", pan: true, fade: true });
  const [extrasDraft, setExtrasDraft] = useState<ExtrasDraft>({ hook: "", tags: "", cta: "" });
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [keyStatus, setKeyStatus] = useState<KeyStatus | null>(null);
  const [keyInput, setKeyInput] = useState("");
  const [settingsMsg, setSettingsMsg] = useState<{ ok: boolean; text: string } | null>(null);
  const [savingKey, setSavingKey] = useState(false);
  const videoRef = useRef<HTMLVideoElement>(null);

  const selectedMedia = selection?.kind === "media" ? media.find((m) => m.path === selection.path) ?? null : null;
  const clipIndex = selection?.kind === "clip" ? clips.findIndex((c) => c.id === selection.id) : -1;
  const selectedClip = clipIndex >= 0 ? clips[clipIndex] : null;
  const previewPath = selectedClip?.sourcePath ?? selectedMedia?.path ?? null;
  const plan = toRenderPlan(clips);

  // Keep the trim inputs in sync with the selected clip.
  useEffect(() => {
    if (selectedClip) {
      setTrimDraft({ in: selectedClip.inPoint.toFixed(2), out: selectedClip.outPoint.toFixed(2) });
    }
    setTrimError(null);
  }, [selectedClip?.id, selectedClip?.inPoint, selectedClip?.outPoint]);

  async function importMedia() {
    const picked = await open({
      multiple: true,
      directory: false,
      filters: [{ name: "Video", extensions: VIDEO_EXTENSIONS }],
    });
    if (!picked) return;
    const paths = Array.isArray(picked) ? picked : [picked];
    setBusy(true);
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
    setBusy(false);
  }

  function addToTimeline(m: MediaInfo) {
    const clip = createClip(m);
    setClips((prev) => [...prev, clip]);
    setSelection({ kind: "clip", id: clip.id });
  }

  function updateClip(next: TimelineClip) {
    setClips((prev) => prev.map((c) => (c.id === next.id ? next : c)));
  }

  function applyTrim(inPoint: number, outPoint: number) {
    if (!selectedClip) return;
    const r = setTrim(selectedClip, inPoint, outPoint);
    if (r.ok) {
      updateClip(r.clip);
      setTrimError(null);
    } else {
      setTrimError(r.error);
    }
  }

  function playhead() {
    return Math.round((videoRef.current?.currentTime ?? 0) * 100) / 100;
  }

  function move(delta: number) {
    if (clipIndex >= 0) setClips((prev) => moveClip(prev, clipIndex, delta));
  }

  function remove() {
    if (!selectedClip) return;
    setClips((prev) => removeClip(prev, selectedClip.id));
    setSelection(null);
  }

  // When previewing a timeline clip, keep playback inside its in/out range.
  function onLoadedMetadata() {
    if (selectedClip && videoRef.current) videoRef.current.currentTime = selectedClip.inPoint;
  }
  function onTimeUpdate() {
    const v = videoRef.current;
    if (!selectedClip || !v) return;
    if (v.currentTime >= selectedClip.outPoint) {
      v.pause();
      v.currentTime = selectedClip.outPoint;
    }
  }
  function onPlay() {
    const v = videoRef.current;
    if (!selectedClip || !v) return;
    if (v.currentTime < selectedClip.inPoint || v.currentTime >= selectedClip.outPoint - 0.05) {
      v.currentTime = selectedClip.inPoint;
    }
  }

  async function exportVideo() {
    if (!plan.ok) return;
    const picked = await save({
      title: "Export MP4",
      defaultPath: "reel.mp4",
      filters: [{ name: "MP4 video", extensions: ["mp4"] }],
    });
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

  async function runAutoCut(m: MediaInfo) {
    await startAutoCut(m.fileName, "auto_cut", { path: m.path });
  }

  async function runAutoCutTimeline() {
    const planned = toRenderPlan(clips);
    if (!planned.ok) {
      setAutoCut({ kind: "error", message: planned.error });
      return;
    }
    const segments = planned.segments.map((sg) => ({ sourcePath: sg.sourcePath, inPoint: sg.inPoint, outPoint: sg.outPoint }));
    await startAutoCut(`Timeline: ${plural(clips.length, "clip")}`, "auto_cut_timeline", { segments });
  }

  async function runTravel() {
    const planned = toRenderPlan(clips);
    if (!planned.ok) {
      setAutoCut({ kind: "error", message: planned.error });
      return;
    }
    const segments = planned.segments.map((sg) => ({ sourcePath: sg.sourcePath, inPoint: sg.inPoint, outPoint: sg.outPoint }));
    const style = { ...travelDraft, music: editStyle.music };
    await startAutoCut(`Travel reel: ${plural(clips.length, "clip")}`, "travel_edit", { segments, style }, false);
  }

  async function startAutoCut(source: string, command: string, args: Record<string, unknown>, withStyle = true) {
    setAutoCut({ kind: "running", source, stage: "Starting…", progress: 0 });
    const offStage = await listen<string>("autocut-stage", (e) =>
      setAutoCut((s) => (s.kind === "running" ? { ...s, stage: e.payload } : s)),
    );
    const offProgress = await listen<number>("autocut-progress", (e) =>
      setAutoCut((s) => (s.kind === "running" ? { ...s, progress: e.payload } : s)),
    );
    try {
      const result = await invoke<AutoCutResult>(command, { ...(withStyle ? { style: editStyle } : {}), ...args, extras: toExtras(extrasDraft) });
      setAutoCut({ kind: "done", result });
    } catch (e) {
      setAutoCut({ kind: "error", message: String(e) });
    } finally {
      offStage();
      offProgress();
    }
  }

  async function chooseMusic() {
    const picked = await open({ multiple: false, directory: false, filters: [{ name: "Audio", extensions: MUSIC_EXTENSIONS }] });
    if (typeof picked === "string") setEditStyle((st) => ({ ...st, music: picked }));
  }

  async function addResultToMedia(path: string) {
    try {
      const info = await invoke<MediaInfo>("probe_media", { path });
      setMedia((prev) => (prev.some((x) => x.path === info.path) ? prev : [...prev, info]));
      setSelection({ kind: "media", path: info.path });
      setAutoCut({ kind: "idle" });
    } catch (e) {
      setAutoCut({ kind: "error", message: String(e) });
    }
  }

  async function openSettings() {
    setSettingsOpen(true);
    setSettingsMsg(null);
    setKeyInput("");
    try {
      setKeyStatus(await invoke<KeyStatus>("api_key_status"));
    } catch (e) {
      setSettingsMsg({ ok: false, text: String(e) });
    }
  }

  function closeSettings() {
    setSettingsOpen(false);
    setKeyInput(""); // never keep the key in memory longer than needed
  }

  async function saveKey() {
    setSavingKey(true);
    setSettingsMsg(null);
    try {
      const status = await invoke<KeyStatus>("save_api_key", { key: keyInput });
      setKeyStatus(status);
      setSettingsMsg({ ok: true, text: "Saved to your macOS Keychain." });
    } catch (e) {
      setSettingsMsg({ ok: false, text: String(e) });
    } finally {
      setKeyInput("");
      setSavingKey(false);
    }
  }

  async function removeKey() {
    setSettingsMsg(null);
    try {
      setKeyStatus(await invoke<KeyStatus>("remove_api_key"));
      setSettingsMsg({ ok: true, text: "Key removed from the Keychain." });
    } catch (e) {
      setSettingsMsg({ ok: false, text: String(e) });
    }
  }

  const keyLabel =
    keyStatus === null
      ? "Checking…"
      : keyStatus.source === "keychain"
        ? "Saved in macOS Keychain ✓"
        : keyStatus.source === "env"
          ? "Set by the ANTHROPIC_API_KEY environment variable ✓"
          : "Not set";

  const total = totalDuration(clips);
  const rendering = exportState.kind === "rendering";

  const timelineMode = clips.length > 0;
  const audioOk = timelineMode
    ? clips.some((c) => media.find((m) => m.path === c.sourcePath)?.hasAudio)
    : !!selectedMedia?.hasAudio;
  const musicRow = (
                <div className="music-row">
                  <span className="muted">Music:</span>
                  <span className="music-name" title={editStyle.music ?? ""}>
                    {editStyle.music ? editStyle.music.split("/").pop() : "none"}
                  </span>
                  <button className="btn small" onClick={chooseMusic}>Choose…</button>
                  {editStyle.music && (
                    <button className="btn small" onClick={() => setEditStyle((st) => ({ ...st, music: null }))} aria-label="Remove music">
                      ✕
                    </button>
                  )}
                </div>
  );
  const extrasBox = (
                <div className="extras-box">
                  <span className="muted">Hook, tags &amp; CTA (optional):</span>
                  <label className="field">
                    Hook (first 3 seconds)
                    <input
                      type="text"
                      placeholder="NANGANGALAY NA KAMAY MO SA 8 HRS NA TRABAHO?"
                      value={extrasDraft.hook}
                      onChange={(e) => setExtrasDraft((d) => ({ ...d, hook: e.target.value }))}
                    />
                  </label>
                  <label className="field">
                    Tags, one per line. Add "| phrase" to show it when that phrase is said
                    <textarea
                      rows={4}
                      placeholder={"LOGITECH ERGONOMIC MOUSE | logitech\nBLUETOOTH + LAPTOP + DESKTOP | bluetooth\nCLEAN WHITE DESIGN"}
                      value={extrasDraft.tags}
                      onChange={(e) => setExtrasDraft((d) => ({ ...d, tags: e.target.value }))}
                    />
                  </label>
                  <label className="field">
                    CTA (last 3.5 seconds)
                    <input
                      type="text"
                      placeholder="CLICK THE YELLOW BASKET"
                      value={extrasDraft.cta}
                      onChange={(e) => setExtrasDraft((d) => ({ ...d, cta: e.target.value }))}
                    />
                  </label>
                </div>
  );
  const travelPanel = (
    <>
      <button
        className="btn primary wide"
        onClick={runTravel}
        disabled={autoCut.kind === "running" || !timelineMode}
        title={timelineMode ? "Join the timeline clips into a vertical travel reel" : "Add clips to the timeline first"}
      >
        {timelineMode ? `🌴 Make travel reel (${plural(clips.length, "clip")})` : "🌴 Make travel reel"}
      </button>
      <p className="muted small-note">
        {timelineMode
          ? "For footage without speech: nothing is cut for silence. Each clip keeps its best seconds, then they are joined with transitions. Add your voice-over afterwards in CapCut."
          : "Add your clips to the timeline with the + button first."}
      </p>
      <div className="travel-grid">
        <label className="field">
          Longest per clip
          <select value={travelDraft.clipSec} onChange={(e) => setTravelDraft((d) => ({ ...d, clipSec: Number(e.target.value) }))}>
            <option value={2.5}>2.5 s (fast)</option>
            <option value={3}>3 s</option>
            <option value={4}>4 s</option>
            <option value={5}>5 s</option>
            <option value={6}>6 s</option>
            <option value={0}>Keep my trims</option>
          </select>
        </label>
        <label className="field">
          Transition
          <select value={travelDraft.transition} onChange={(e) => setTravelDraft((d) => ({ ...d, transition: e.target.value as TravelDraft["transition"] }))}>
            <option value="smooth">Smooth fade</option>
            <option value="dynamic">Dynamic mix</option>
            <option value="none">None (hard cut)</option>
          </select>
        </label>
        <label className="field">
          Color look
          <select value={travelDraft.grade} onChange={(e) => setTravelDraft((d) => ({ ...d, grade: e.target.value as TravelDraft["grade"] }))}>
            <option value="warm">Warm travel</option>
            <option value="vivid">Vivid</option>
            <option value="natural">Natural</option>
          </select>
        </label>
      </div>
      <div className="style-options">
        <label className="check">
          <input type="checkbox" checked={travelDraft.pan} onChange={(e) => setTravelDraft((d) => ({ ...d, pan: e.target.checked }))} />
          Slow drift on each clip
        </label>
        <label className="check">
          <input type="checkbox" checked={travelDraft.fade} onChange={(e) => setTravelDraft((d) => ({ ...d, fade: e.target.checked }))} />
          Fade in and out
        </label>
      </div>
      {musicRow}
      {extrasBox}
    </>
  );
  const autoEditBox = (
          <div className="auto-cut-box">
            <div className="mode-row" role="group" aria-label="Edit mode">
              <button className={`btn small${editMode === "talk" ? " active" : ""}`} onClick={() => setEditMode("talk")} aria-pressed={editMode === "talk"}>
                Talking video
              </button>
              <button className={`btn small${editMode === "travel" ? " active" : ""}`} onClick={() => setEditMode("travel")} aria-pressed={editMode === "travel"}>
                Travel b-roll
              </button>
            </div>
            {editMode === "travel" ? travelPanel : (
            <>
            <button
              className="btn primary wide"
              onClick={timelineMode ? runAutoCutTimeline : () => selectedMedia && runAutoCut(selectedMedia)}
              disabled={autoCut.kind === "running" || !audioOk}
              title={audioOk ? "Cut, zoom, add sounds and captions automatically" : "Needs a video with speech"}
            >
              {timelineMode ? `✨ Auto Edit timeline (${plural(clips.length, "clip")})` : "✨ Auto Edit (free, no AI)"}
            </button>
            {timelineMode && (
              <p className="muted small-note">Joins every clip on the timeline in order (with your trims), then edits it as one reel.</p>
            )}
            {audioOk ? (
              <>
                <div className="style-options">
                  {([
                    ["zoom", "Zoom on cuts"],
                    ["soundEffects", "Whoosh on cuts"],
                    ["highlightWords", "Highlight spoken word"],
                    ["loudness", "Even out volume"],
                  ] as const).map(([key, label]) => (
                    <label key={key} className="check">
                      <input
                        type="checkbox"
                        checked={editStyle[key]}
                        onChange={(e) => setEditStyle((st) => ({ ...st, [key]: e.target.checked }))}
                      />
                      {label}
                    </label>
                  ))}
                </div>
                {musicRow}
                {extrasBox}
                <p className="muted small-note">
                  Cuts pauses, um/uh and repeats, then makes a 9:16 reel with captions. Runs on this Mac. Use music you
                  have the rights to.
                </p>
              </>
            ) : (
              <p className="muted small-note">This video has no audio, so there is nothing to transcribe.</p>
            )}
            </>
            )}
          </div>
  );

  return (
    <div className="app">
      <header className="topbar">
        <span className="project-name">Untitled project</span>
        <div className="topbar-actions">
        <button className="btn" onClick={openSettings}>
          Settings
        </button>
        <button
          className="btn primary"
          onClick={exportVideo}
          disabled={!plan.ok || rendering}
          title={plan.ok ? "Export the timeline to MP4" : plan.error}
        >
          {rendering ? "Exporting…" : "Export"}
        </button>
        </div>
      </header>

      <aside className="panel media-bin">
        <div className="panel-header">
          <h2>Media</h2>
          <button className="btn" onClick={importMedia} disabled={busy}>
            {busy ? "Importing…" : "Import"}
          </button>
        </div>
        {media.length === 0 && <p className="muted">Import a video to begin.</p>}
        <ul>
          {media.map((m) => (
            <li key={m.path} className="media-row">
              <button
                className={`media-item ${selection?.kind === "media" && selection.path === m.path ? "active" : ""}`}
                onClick={() => setSelection({ kind: "media", path: m.path })}
                title={m.path}
              >
                <span className="name">{m.fileName}</span>
                <span className="muted">{formatTime(m.durationSec)}</span>
              </button>
              <button className="btn icon" onClick={() => addToTimeline(m)} title="Add to timeline" aria-label={`Add ${m.fileName} to timeline`}>
                +
              </button>
            </li>
          ))}
        </ul>
        {errors.length > 0 && (
          <div className="errors" role="alert">
            {errors.map((e) => (
              <p key={e}>{e}</p>
            ))}
          </div>
        )}
      </aside>

      <main className="preview">
        {previewPath ? (
          <video
            key={`${previewPath}|${selectedClip?.id ?? "media"}`}
            ref={videoRef}
            src={convertFileSrc(previewPath)}
            controls
            onLoadedMetadata={onLoadedMetadata}
            onTimeUpdate={onTimeUpdate}
            onPlay={onPlay}
          />
        ) : (
          <p className="muted">No clip selected</p>
        )}
      </main>

      <aside className="panel properties">
        <h2>Properties</h2>
        {selectedClip ? (
          <div className="clip-props">
            <dl>
              <dt>Clip</dt><dd>#{clipIndex + 1} of {clips.length}</dd>
              <dt>Source</dt><dd title={selectedClip.sourcePath}>{selectedClip.fileName}</dd>
              <dt>Source length</dt><dd>{formatTime(selectedClip.sourceDuration)}</dd>
              <dt>Trimmed length</dt><dd>{formatTime(clipLength(selectedClip))}</dd>
            </dl>
            <form
              className="trim-form"
              onSubmit={(e) => {
                e.preventDefault();
                applyTrim(parseFloat(trimDraft.in), parseFloat(trimDraft.out));
              }}
            >
              <label>
                In (s)
                <input type="number" step="0.01" min="0" value={trimDraft.in}
                  onChange={(e) => setTrimDraft((d) => ({ ...d, in: e.target.value }))} />
              </label>
              <label>
                Out (s)
                <input type="number" step="0.01" min="0" value={trimDraft.out}
                  onChange={(e) => setTrimDraft((d) => ({ ...d, out: e.target.value }))} />
              </label>
              <button className="btn" type="submit">Apply trim</button>
            </form>
            <div className="btn-row">
              <button className="btn" onClick={() => applyTrim(playhead(), selectedClip.outPoint)}>Set in at playhead</button>
              <button className="btn" onClick={() => applyTrim(selectedClip.inPoint, playhead())}>Set out at playhead</button>
            </div>
            {trimError && <p className="errors" role="alert">{trimError}</p>}
            <div className="btn-row">
              <button className="btn" onClick={() => move(-1)} disabled={clipIndex === 0}>◀ Earlier</button>
              <button className="btn" onClick={() => move(1)} disabled={clipIndex === clips.length - 1}>Later ▶</button>
              <button className="btn danger" onClick={remove}>Remove</button>
            </div>
          </div>
        ) : selectedMedia ? (
          <>
          <dl>
            <dt>File</dt><dd title={selectedMedia.path}>{selectedMedia.fileName}</dd>
            <dt>Duration</dt><dd>{formatTime(selectedMedia.durationSec)}</dd>
            <dt>Resolution</dt><dd>{selectedMedia.width} × {selectedMedia.height}</dd>
            <dt>Codec</dt><dd>{selectedMedia.videoCodec}</dd>
            <dt>Frame rate</dt><dd>{selectedMedia.fps.toFixed(2)} fps</dd>
            <dt>Audio</dt><dd>{selectedMedia.hasAudio ? "Yes" : "None"}</dd>
            <dt>Size</dt><dd>{formatSize(selectedMedia.sizeBytes)}</dd>
          </dl>
          </>
        ) : (
          <p className="muted">Select a clip to see its details.</p>
        )}
        {(timelineMode || selectedMedia) && autoEditBox}
      </aside>

      <footer className="panel timeline">
        <div className="panel-header">
          <h2>Timeline</h2>
          <span className="muted">{clips.length} clip{clips.length === 1 ? "" : "s"} · {formatTime(total)}</span>
        </div>
        {clips.length === 0 ? (
          <p className="muted">Use + in the media bin to add clips.</p>
        ) : (
          <div className="track">
            {clips.map((c, i) => (
              <button
                key={c.id}
                className={`track-clip ${c.id === selectedClip?.id ? "active" : ""}`}
                style={{ flexGrow: clipLength(c) }}
                onClick={() => setSelection({ kind: "clip", id: c.id })}
                title={`${c.fileName} ${c.inPoint.toFixed(2)}–${c.outPoint.toFixed(2)}s`}
              >
                <span className="order">{i + 1}</span>
                <span className="name">{c.fileName}</span>
                <span className="muted">{clipLength(c).toFixed(2)}s</span>
              </button>
            ))}
          </div>
        )}
        <div className="render-plan" aria-label="Render plan">
          <span className="muted">Render plan:</span>{" "}
          {plan.ok
            ? plan.segments.map((s) => `${s.order}. ${s.sourcePath.split("/").pop()} ${s.inPoint.toFixed(2)}→${s.outPoint.toFixed(2)}`).join("   ")
            : plan.error}
        </div>
      </footer>

      {autoCut.kind !== "idle" && (
        <div className="modal-backdrop">
          <div className="modal" role="dialog" aria-modal="true" aria-labelledby="autocut-title">
            <h2 id="autocut-title">Auto Edit</h2>
            {autoCut.kind === "running" && (
              <>
                <p>{autoCut.source}</p>
                <p className="muted">{autoCut.stage}</p>
                <progress max={1} value={autoCut.stage === "Rendering…" ? autoCut.progress : undefined} />
                <p className="muted small-note">Transcription and rendering run on this Mac. Nothing is uploaded.</p>
              </>
            )}
            {autoCut.kind === "done" && (
              <>
                <p className="success">Reel ready</p>
                <dl>
                  <dt>Length</dt>
                  <dd>
                    {formatTime(autoCut.result.durationSec)} (from {formatTime(autoCut.result.sourceDurationSec)})
                  </dd>
                  {autoCut.result.mode === "travel" ? (
                    <>
                      <dt>Clips</dt><dd>{autoCut.result.segments}</dd>
                    </>
                  ) : (
                    <>
                      <dt>Removed</dt>
                      <dd>
                        {plural(autoCut.result.pausesRemoved, "pause")} · {plural(autoCut.result.fillersRemoved, "filler")} ·{" "}
                        {plural(autoCut.result.repeatsRemoved, "repeat")}
                      </dd>
                      <dt>Captions</dt><dd>{autoCut.result.captions}</dd>
                      <dt>Check</dt>
                      <dd>{Math.round(autoCut.result.keptWordsMatch * 100)}% of kept words heard in the result</dd>
                    </>
                  )}
                </dl>
                <div className="btn-row">
                  <button className="btn primary" onClick={() => addResultToMedia(autoCut.result.outputPath)}>
                    Add to media & preview
                  </button>
                  <button className="btn" onClick={() => revealItemInDir(autoCut.result.outputPath)}>Show in Finder</button>
                  <button className="btn" onClick={() => setAutoCut({ kind: "idle" })}>Close</button>
                </div>
              </>
            )}
            {autoCut.kind === "error" && (
              <>
                <p className="errors" role="alert">{autoCut.message}</p>
                <div className="btn-row">
                  <button className="btn" onClick={() => setAutoCut({ kind: "idle" })}>Close</button>
                </div>
              </>
            )}
          </div>
        </div>
      )}

      {settingsOpen && (
        <div className="modal-backdrop" onClick={(e) => e.target === e.currentTarget && closeSettings()}>
          <div className="modal settings" role="dialog" aria-modal="true" aria-labelledby="settings-title">
            <h2 id="settings-title">Settings</h2>
            <h3>Claude API key</h3>
            <p className="key-status">
              Status: <strong>{keyLabel}</strong>
            </p>
            <form
              onSubmit={(e) => {
                e.preventDefault();
                if (keyInput.trim()) saveKey();
              }}
            >
              <label className="field">
                {keyStatus?.source === "keychain" ? "Replace key" : "Paste your API key"}
                <input
                  type="password"
                  autoComplete="off"
                  spellCheck={false}
                  placeholder="sk-ant-…"
                  value={keyInput}
                  onChange={(e) => setKeyInput(e.target.value)}
                  autoFocus
                />
              </label>
              <div className="btn-row">
                <button className="btn primary" type="submit" disabled={!keyInput.trim() || savingKey}>
                  {savingKey ? "Saving…" : "Save key"}
                </button>
                {keyStatus?.source === "keychain" && (
                  <button className="btn danger" type="button" onClick={removeKey}>
                    Remove key
                  </button>
                )}
                <button className="btn" type="button" onClick={closeSettings}>
                  Close
                </button>
              </div>
            </form>
            {settingsMsg && (
              <p className={settingsMsg.ok ? "success" : "errors"} role="status">
                {settingsMsg.text}
              </p>
            )}
            <p className="muted small">
              The key is stored only in your macOS login Keychain (item “ANTHROPIC_API_KEY”), never in app files.
              It is used only for requests to api.anthropic.com, which are billed through your Claude Console account.
            </p>
          </div>
        </div>
      )}

      {exportState.kind !== "idle" && (
        <div className="modal-backdrop">
          <div className="modal" role="dialog" aria-modal="true" aria-labelledby="export-title">
            <h2 id="export-title">Export MP4</h2>
            {exportState.kind === "rendering" && (
              <>
                <p title={exportState.outputPath}>Rendering {exportState.outputPath.split("/").pop()}…</p>
                <progress max={1} value={exportState.progress} />
                <p className="muted">{Math.round(exportState.progress * 100)}%</p>
              </>
            )}
            {exportState.kind === "done" && (
              <>
                <p className="success">Export complete</p>
                <dl>
                  <dt>File</dt><dd title={exportState.result.outputPath}>{exportState.result.outputPath.split("/").pop()}</dd>
                  <dt>Duration</dt><dd>{formatTime(exportState.result.durationSec)}</dd>
                  <dt>Resolution</dt><dd>{exportState.result.width} × {exportState.result.height}</dd>
                  <dt>Audio</dt><dd>{exportState.result.hasAudio ? "Yes" : "None"}</dd>
                  <dt>Size</dt><dd>{formatSize(exportState.result.sizeBytes)}</dd>
                </dl>
                <div className="btn-row">
                  <button className="btn" onClick={() => revealItemInDir(exportState.result.outputPath)}>Show in Finder</button>
                  <button className="btn primary" onClick={() => setExportState({ kind: "idle" })}>Done</button>
                </div>
              </>
            )}
            {exportState.kind === "error" && (
              <>
                <p className="errors" role="alert">{exportState.message}</p>
                <div className="btn-row">
                  <button className="btn" onClick={() => setExportState({ kind: "idle" })}>Close</button>
                </div>
              </>
            )}
          </div>
        </div>
      )}
    </div>
  );
}

export default App;
