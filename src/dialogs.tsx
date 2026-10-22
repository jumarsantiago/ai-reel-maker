// Dialogs: progress, results, caption review, previews, batch, settings and timeline export.
import { useEffect, useState } from "react";
import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import { Modal } from "./ui.tsx";

export function formatTime(sec: number) {
  const m = Math.floor(sec / 60);
  const s = (sec % 60).toFixed(1).padStart(4, "0");
  return `${m}:${s}`;
}

export function plural(n: number, word: string) {
  return `${n} ${word}${n === 1 ? "" : "s"}`;
}

export function formatSize(bytes: number) {
  return bytes >= 1e9 ? `${(bytes / 1e9).toFixed(2)} GB` : `${(bytes / 1e6).toFixed(1)} MB`;
}

const fileName = (p: string) => p.split("/").pop() ?? p;

export type AutoCutResult = {
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
  srtPath: string | null;
  otherOutputPath: string | null;
};

export function ProgressDialog(props: { title: string; source: string; stage: string; progress: number }) {
  const rendering = props.stage.startsWith("Rendering");
  return (
    <Modal title={props.title}>
      <p className="strong ellipsis">{props.source}</p>
      <p className="muted">{props.stage}</p>
      <progress max={1} value={rendering ? props.progress : undefined} />
      <p className="note">Everything runs on this Mac. Nothing is uploaded.</p>
    </Modal>
  );
}

export function ErrorDialog(props: { message: string; onClose: () => void }) {
  return (
    <Modal title="Something went wrong" onClose={props.onClose} footer={<button className="btn" onClick={props.onClose}>Close</button>}>
      <pre className="error-text" role="alert">{props.message}</pre>
    </Modal>
  );
}

export function ResultDialog(props: {
  result: AutoCutResult;
  thumbnailText: string;
  thumbnailStyle: unknown;
  font: string | null;
  onAddToMedia: (path: string) => void;
  onClose: () => void;
}) {
  const r = props.result;
  const [thumb, setThumb] = useState<{ kind: "idle" } | { kind: "busy" } | { kind: "done"; path: string } | { kind: "error"; message: string }>({ kind: "idle" });

  async function makeThumbnail() {
    setThumb({ kind: "busy" });
    try {
      const path = await invoke<string>("make_thumbnail", {
        videoPath: r.outputPath,
        text: props.thumbnailText || null,
        font: props.font,
        style: r.mode === "talk" ? props.thumbnailStyle : null,
      });
      setThumb({ kind: "done", path });
    } catch (e) {
      setThumb({ kind: "error", message: String(e) });
    }
  }

  return (
    <Modal
      title="Reel ready"
      wide
      onClose={props.onClose}
      footer={
        <>
          <button className="btn" onClick={() => revealItemInDir(r.outputPath)}>Show in Finder</button>
          <button className="btn primary" onClick={() => props.onAddToMedia(r.outputPath)}>Open in preview</button>
        </>
      }
    >
      <div className="result-grid">
        <video className="result-video" src={convertFileSrc(r.outputPath)} controls />
        <div className="result-info">
          <dl>
            <dt>Length</dt>
            <dd>{formatTime(r.durationSec)} <span className="muted">from {formatTime(r.sourceDurationSec)}</span></dd>
            {r.mode === "travel" ? (
              <>
                <dt>Clips</dt><dd>{r.segments}</dd>
              </>
            ) : (
              <>
                <dt>Removed</dt>
                <dd>{plural(r.pausesRemoved, "pause")}, {plural(r.fillersRemoved, "filler")}, {plural(r.repeatsRemoved, "repeat")}</dd>
                <dt>Captions</dt><dd>{r.captions}</dd>
                <dt>Check</dt><dd>{Math.round(r.keptWordsMatch * 100)}% of the kept words are heard in the result</dd>
              </>
            )}
          </dl>
          <div className="file-list">
            <FileLine label="Video" path={r.outputPath} />
            {r.otherOutputPath && <FileLine label="Other shape" path={r.otherOutputPath} />}
            {r.srtPath && <FileLine label="Captions (.srt)" path={r.srtPath} />}
          </div>
          <div className="thumb-box">
            {thumb.kind === "done" ? (
              <>
                <img className="thumb" src={convertFileSrc(thumb.path)} alt="Thumbnail" />
                <FileLine label="Thumbnail" path={thumb.path} />
              </>
            ) : (
              <button className="btn" onClick={makeThumbnail} disabled={thumb.kind === "busy"}>
                {thumb.kind === "busy" ? "Making thumbnail…" : "🖼 Make a thumbnail"}
              </button>
            )}
            {thumb.kind === "error" && <p className="error-text">{thumb.message}</p>}
            {thumb.kind === "idle" && (
              <p className="note">Picks the sharpest moment{props.thumbnailText ? " and adds your hook as the title" : ""}.</p>
            )}
          </div>
        </div>
      </div>
    </Modal>
  );
}

function FileLine(props: { label: string; path: string }) {
  return (
    <div className="file-line">
      <span className="muted">{props.label}</span>
      <span className="ellipsis" title={props.path}>{fileName(props.path)}</span>
      <button className="link-btn" onClick={() => revealItemInDir(props.path)}>Show</button>
    </div>
  );
}

export type ReviewCaption = { index: number; text: string; start: number; end: number; original: string; deleted: boolean };
export type ReviewState = { dir: string; sourceDurationSec: number; label: string; captions: ReviewCaption[] };

export function reviewEdits(r: ReviewState) {
  return r.captions
    .filter((c) => c.deleted || c.text.trim() !== c.original)
    .map((c) => ({ index: c.index, text: c.text, deleted: c.deleted }));
}

export function ReviewDialog(props: {
  review: ReviewState;
  setReview: (r: ReviewState) => void;
  onRender: () => void;
  onCancel: () => void;
  onPreview: () => Promise<string>;
}) {
  const { review } = props;
  const [preview, setPreview] = useState<{ kind: "idle" } | { kind: "busy" } | { kind: "ready"; path: string; at: number } | { kind: "error"; message: string }>({ kind: "idle" });
  const changed = reviewEdits(review).length;
  const kept = review.captions.filter((c) => !c.deleted).length;
  const set = (index: number, patch: Partial<ReviewCaption>) =>
    props.setReview({ ...review, captions: review.captions.map((x) => (x.index === index ? { ...x, ...patch } : x)) });

  async function runPreview() {
    setPreview({ kind: "busy" });
    try {
      const path = await props.onPreview();
      setPreview({ kind: "ready", path, at: Date.now() });
    } catch (e) {
      setPreview({ kind: "error", message: String(e) });
    }
  }

  return (
    <Modal
      title="Check the captions"
      wide
      onClose={props.onCancel}
      footer={
        <>
          <span className="note grow">{kept} captions · {changed === 0 ? "no changes" : plural(changed, "change")}</span>
          <button className="btn" onClick={runPreview} disabled={preview.kind === "busy"}>
            {preview.kind === "busy" ? "Making preview…" : "▶ Preview 10 s"}
          </button>
          <button className="btn primary" onClick={props.onRender}>Render reel</button>
        </>
      }
    >
      <p className="note">Fix any spelling, or remove a caption with ✕. The timing stays the same.</p>
      <div className="review-layout">
        <div className="review-list">
          {review.captions.map((c, i) => (
            <div key={c.index} className={`review-row${c.deleted ? " deleted" : ""}${!c.deleted && c.text.trim() !== c.original ? " edited" : ""}`}>
              <span className="review-time">{formatTime(c.start)}</span>
              <input
                type="text"
                value={c.text}
                disabled={c.deleted}
                aria-label={`Caption ${i + 1}`}
                onChange={(e) => set(c.index, { text: e.target.value })}
              />
              <button
                type="button"
                className="icon-btn"
                title={c.deleted ? "Put this caption back" : "Remove this caption"}
                aria-label={c.deleted ? "Put this caption back" : "Remove this caption"}
                onClick={() => set(c.index, { deleted: !c.deleted })}
              >
                {c.deleted ? "↩" : "✕"}
              </button>
            </div>
          ))}
        </div>
        {preview.kind !== "idle" && (
          <div className="review-preview">
            {preview.kind === "ready" && (
              <video key={preview.at} src={`${convertFileSrc(preview.path)}?t=${preview.at}`} controls autoPlay />
            )}
            {preview.kind === "busy" && <div className="preview-wait">Making a 10-second preview…</div>}
            {preview.kind === "error" && <p className="error-text">{preview.message}</p>}
          </div>
        )}
      </div>
    </Modal>
  );
}

export function PreviewDialog(props: { path: string; onClose: () => void }) {
  const [at] = useState(Date.now());
  return (
    <Modal title="Preview — first 10 seconds" onClose={props.onClose} footer={<button className="btn primary" onClick={props.onClose}>Close</button>}>
      <video className="preview-video" src={`${convertFileSrc(props.path)}?t=${at}`} controls autoPlay />
      <p className="note">A quick, lower-quality version. The final reel is sharper.</p>
    </Modal>
  );
}

export type BatchItem = { path: string; name: string; status: "waiting" | "running" | "done" | "error"; output?: string; error?: string; stage?: string };

export function BatchDialog(props: { items: BatchItem[]; running: boolean; onClose: () => void }) {
  const done = props.items.filter((i) => i.status === "done").length;
  return (
    <Modal
      title={props.running ? `Editing ${props.items.length} videos…` : "Batch finished"}
      wide
      onClose={props.running ? undefined : props.onClose}
      footer={!props.running && <button className="btn primary" onClick={props.onClose}>Close</button>}
    >
      <p className="note">{done} of {props.items.length} done. Each video is edited with the current settings, one after another.</p>
      <ul className="batch-list">
        {props.items.map((it) => (
          <li key={it.path} className={`batch-row ${it.status}`}>
            <span className="badge">{{ waiting: "Waiting", running: "Working", done: "Done", error: "Failed" }[it.status]}</span>
            <span className="ellipsis" title={it.path}>{it.name}</span>
            <span className="muted ellipsis">{it.status === "running" ? it.stage : it.status === "error" ? it.error : ""}</span>
            {it.output && <button className="link-btn" onClick={() => revealItemInDir(it.output!)}>Show</button>}
          </li>
        ))}
      </ul>
    </Modal>
  );
}

type KeyStatus = { source: "env" | "keychain" | "none"; account: string };
type Fix = { from: string; to: string };

export function SettingsDialog(props: { onClose: () => void }) {
  const [keyStatus, setKeyStatus] = useState<KeyStatus | null>(null);
  const [keyInput, setKeyInput] = useState("");
  const [msg, setMsg] = useState<{ ok: boolean; text: string } | null>(null);
  const [saving, setSaving] = useState(false);
  const [fixes, setFixes] = useState<Fix[] | null>(null);
  const [dictMsg, setDictMsg] = useState<string | null>(null);

  useEffect(() => {
    invoke<KeyStatus>("api_key_status").then(setKeyStatus).catch((e) => setMsg({ ok: false, text: String(e) }));
    invoke<Fix[]>("get_dictionary").then(setFixes).catch(() => setFixes([]));
  }, []);

  async function saveKey() {
    setSaving(true);
    setMsg(null);
    try {
      setKeyStatus(await invoke<KeyStatus>("save_api_key", { key: keyInput }));
      setMsg({ ok: true, text: "Saved to your macOS Keychain." });
    } catch (e) {
      setMsg({ ok: false, text: String(e) });
    } finally {
      setKeyInput(""); // never keep the key in memory longer than needed
      setSaving(false);
    }
  }

  async function removeKey() {
    setMsg(null);
    try {
      setKeyStatus(await invoke<KeyStatus>("remove_api_key"));
      setMsg({ ok: true, text: "Key removed from the Keychain." });
    } catch (e) {
      setMsg({ ok: false, text: String(e) });
    }
  }

  async function saveDictionary(next: Fix[]) {
    setFixes(next);
    try {
      await invoke("set_dictionary", { fixes: next.filter((f) => f.from.trim() && f.to.trim()) });
      setDictMsg("Saved.");
    } catch (e) {
      setDictMsg(String(e));
    }
  }

  const keyLabel =
    keyStatus === null ? "Checking…"
      : keyStatus.source === "keychain" ? "Saved in macOS Keychain ✓"
        : keyStatus.source === "env" ? "Set by the ANTHROPIC_API_KEY environment variable ✓"
          : "Not set";

  return (
    <Modal title="Settings" wide onClose={() => { setKeyInput(""); props.onClose(); }}>
      <section className="settings-section">
        <h3>Spelling fixes</h3>
        <p className="note">Corrections you made in the caption check. They are applied to new videos automatically.</p>
        {fixes === null ? (
          <p className="muted">Loading…</p>
        ) : (
          <div className="dict">
            {fixes.length === 0 && <p className="muted">None yet. Fix a caption in the caption check and it will show up here.</p>}
            {fixes.map((f, i) => (
              <div key={i} className="dict-row">
                <input aria-label="Heard as" value={f.from} onChange={(e) => setFixes(fixes.map((x, j) => (j === i ? { ...x, from: e.target.value } : x)))} />
                <span className="muted">→</span>
                <input aria-label="Correct spelling" value={f.to} onChange={(e) => setFixes(fixes.map((x, j) => (j === i ? { ...x, to: e.target.value } : x)))} />
                <button className="icon-btn" aria-label="Delete this fix" onClick={() => saveDictionary(fixes.filter((_, j) => j !== i))}>✕</button>
              </div>
            ))}
            <div className="btn-row">
              <button className="btn small" onClick={() => setFixes([...fixes, { from: "", to: "" }])}>+ Add a fix</button>
              <button className="btn small primary" onClick={() => saveDictionary(fixes)}>Save fixes</button>
              {dictMsg && <span className="note">{dictMsg}</span>}
            </div>
          </div>
        )}
      </section>

      <section className="settings-section">
        <h3>Claude API key (optional)</h3>
        <p className="note">Only for the paid Claude planner. Auto Edit and Travel mode never need it.</p>
        <p>Status: <strong>{keyLabel}</strong></p>
        <form
          onSubmit={(e) => {
            e.preventDefault();
            if (keyInput.trim()) saveKey();
          }}
        >
          <div className="field">
            <label htmlFor="api-key">{keyStatus?.source === "keychain" ? "Replace key" : "Paste your API key"}</label>
            <input id="api-key" type="password" autoComplete="off" spellCheck={false} placeholder="sk-ant-…" value={keyInput} onChange={(e) => setKeyInput(e.target.value)} />
          </div>
          <div className="btn-row">
            <button className="btn primary small" type="submit" disabled={!keyInput.trim() || saving}>{saving ? "Saving…" : "Save key"}</button>
            {keyStatus?.source === "keychain" && <button className="btn small danger" type="button" onClick={removeKey}>Remove key</button>}
          </div>
        </form>
        {msg && <p className={msg.ok ? "success" : "error-text"} role="status">{msg.text}</p>}
        <p className="note">Stored only in your macOS login Keychain, and used only for requests to api.anthropic.com (billed through your Claude Console account).</p>
      </section>
    </Modal>
  );
}

export type ExportResult = { outputPath: string; durationSec: number; width: number; height: number; hasAudio: boolean; sizeBytes: number };
export type ExportState =
  | { kind: "idle" }
  | { kind: "rendering"; outputPath: string; progress: number }
  | { kind: "done"; result: ExportResult }
  | { kind: "error"; message: string };

export function ExportDialog(props: { state: ExportState; onClose: () => void }) {
  const st = props.state;
  if (st.kind === "idle") return null;
  if (st.kind === "rendering") {
    return (
      <Modal title="Exporting the timeline">
        <p className="ellipsis" title={st.outputPath}>{fileName(st.outputPath)}</p>
        <progress max={1} value={st.progress} />
        <p className="muted">{Math.round(st.progress * 100)}%</p>
      </Modal>
    );
  }
  if (st.kind === "error") return <ErrorDialog message={st.message} onClose={props.onClose} />;
  const r = st.result;
  return (
    <Modal
      title="Export complete"
      onClose={props.onClose}
      footer={
        <>
          <button className="btn" onClick={() => revealItemInDir(r.outputPath)}>Show in Finder</button>
          <button className="btn primary" onClick={props.onClose}>Done</button>
        </>
      }
    >
      <dl>
        <dt>File</dt><dd className="ellipsis" title={r.outputPath}>{fileName(r.outputPath)}</dd>
        <dt>Length</dt><dd>{formatTime(r.durationSec)}</dd>
        <dt>Size</dt><dd>{r.width} × {r.height} · {formatSize(r.sizeBytes)}</dd>
        <dt>Audio</dt><dd>{r.hasAudio ? "Yes" : "None"}</dd>
      </dl>
    </Modal>
  );
}
