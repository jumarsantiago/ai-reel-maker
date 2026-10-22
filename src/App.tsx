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

type Selection = { kind: "media"; path: string } | { kind: "clip"; id: string } | null;

const VIDEO_EXTENSIONS = ["mp4", "mov", "m4v", "webm", "mkv"];

function formatTime(sec: number) {
  const m = Math.floor(sec / 60);
  const s = (sec % 60).toFixed(2).padStart(5, "0");
  return `${m}:${s}`;
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

  const total = totalDuration(clips);
  const rendering = exportState.kind === "rendering";

  return (
    <div className="app">
      <header className="topbar">
        <span className="project-name">Untitled project</span>
        <button
          className="btn primary"
          onClick={exportVideo}
          disabled={!plan.ok || rendering}
          title={plan.ok ? "Export the timeline to MP4" : plan.error}
        >
          {rendering ? "Exporting…" : "Export"}
        </button>
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
          <dl>
            <dt>File</dt><dd title={selectedMedia.path}>{selectedMedia.fileName}</dd>
            <dt>Duration</dt><dd>{formatTime(selectedMedia.durationSec)}</dd>
            <dt>Resolution</dt><dd>{selectedMedia.width} × {selectedMedia.height}</dd>
            <dt>Codec</dt><dd>{selectedMedia.videoCodec}</dd>
            <dt>Frame rate</dt><dd>{selectedMedia.fps.toFixed(2)} fps</dd>
            <dt>Audio</dt><dd>{selectedMedia.hasAudio ? "Yes" : "None"}</dd>
            <dt>Size</dt><dd>{formatSize(selectedMedia.sizeBytes)}</dd>
          </dl>
        ) : (
          <p className="muted">Select a clip to see its details.</p>
        )}
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
