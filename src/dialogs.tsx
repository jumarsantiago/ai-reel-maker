// Dialogs: progress, results, caption review, previews, batch, settings and timeline export.
import { useEffect, useState } from "react";
import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import { open } from "@tauri-apps/plugin-dialog";
import { Modal, Segmented, Toggle } from "./ui.tsx";

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
  useKit: boolean;
  kitDir: string | null;
  result: AutoCutResult;
  thumbnailText: string;
  thumbnailStyle: unknown;
  font: string | null;
  onAddToMedia: (path: string) => void;
  onClose: () => void;
}) {
  const r = props.result;
  const [thumb, setThumb] = useState<{ kind: "idle" } | { kind: "busy" } | { kind: "done"; path: string } | { kind: "error"; message: string }>({ kind: "idle" });

  const [review, setReview] = useState<{ kind: "idle" } | { kind: "busy" } | { kind: "done" } | { kind: "error"; message: string }>({ kind: "idle" });

  async function openReview() {
    setReview({ kind: "busy" });
    try {
      await invoke<string>("kit_edl_review", { projectDir: r.projectDir, videoPath: r.outputPath, kitDir: props.kitDir, useKit: props.useKit });
      setReview({ kind: "done" });
    } catch (e) {
      setReview({ kind: "error", message: String(e) });
    }
  }

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
          {r.mode === "talk" && (
            <div className="thumb-box">
              <button className="btn" onClick={openReview} disabled={!props.useKit || review.kind === "busy"} title={props.useKit ? "" : "Turn on \"Use herk-edit-kit\" in Settings"}>
                {review.kind === "busy" ? "Building review…" : "🔍 Review what was cut (herk-edit-kit)"}
              </button>
              <p className="note">{props.useKit ? "Opens a page in your browser: kept speech in green, every cut in red, original and edited side by side." : "Needs herk-edit-kit, which is turned off."}</p>
              {review.kind === "error" && <p className="error-text">{review.message}</p>}
            </div>
          )}
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
export type ReviewState = { dir: string; sourceDurationSec: number; label: string; captions: ReviewCaption[]; claudePlan?: boolean };

export function reviewEdits(r: ReviewState) {
  return r.captions
    .filter((c) => c.deleted || c.text.trim() !== c.original)
    .map((c) => ({ index: c.index, text: c.text, deleted: c.deleted }));
}

export function ReviewDialog(props: {
  /** Shown only when "Use Claude AI" is on. */
  onAskClaude?: () => void;
  onUndoClaude?: (() => Promise<void>) | null;
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
          {props.onUndoClaude && <button className="btn" onClick={() => props.onUndoClaude?.()}>↩ Undo Claude's plan</button>}
          {props.onAskClaude && <button className="btn" onClick={props.onAskClaude} title="Online, paid. Shows the price first.">✦ Plan with Claude…</button>}
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

export function PreviewDialog(props: { path: string; title?: string; onClose: () => void }) {
  const [at] = useState(Date.now());
  return (
    <Modal title={props.title ?? "Preview — first 10 seconds"} onClose={props.onClose} footer={<button className="btn primary" onClick={props.onClose}>Close</button>}>
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

export type EngineStatus = {
  kit: {
    enabled: boolean;
    found: boolean;
    dir: string | null;
    nodeVersion: string | null;
    hyperframesVersion: string | null;
    tools: { name: string; what: string; present: boolean }[];
    message: string;
  };
  claude: { enabled: boolean; keySource: string; callsMade: number; spentUsd: number; maxCalls: number; maxUsd: number; message: string };
  claudeCode: { installed: boolean; path: string | null; version: string | null; loggedIn: boolean; authMethod: string | null; message: string } | null;
};

type ClaudeEstimate = {
  model: string;
  inputTokens: number;
  worstCaseUsd: number;
  spentUsd: number;
  callsMade: number;
  maxCalls: number;
  maxUsd: number;
  allowed: boolean;
  blockedReason: string | null;
};

type ClaudePlanResult = {
  outcome: { applied: boolean; errors: string[]; costUsd: number; inputTokens: number; outputTokens: number; modelServed: string; segments: number; captions: number };
  captions: { index: number; text: string; start: number; end: number }[] | null;
};

const usd = (n: number) => `$${n.toFixed(n < 0.1 ? 4 : 2)}`;

/** Ask Claude to plan this edit: the exact price first, then one paid call only after the user confirms it. */
export function ClaudePlanDialog(props: {
  method: "code" | "api";
  onMethod: (m: "code" | "api") => void;
  projectDir: string;
  onApplied: (captions: { index: number; text: string; start: number; end: number }[]) => void;
  onClose: () => void;
}) {
  type Cli = { installed: boolean; loggedIn: boolean; version: string | null; message: string };
  const [step, setStep] = useState<
    | { kind: "estimating" }
    | { kind: "ready"; est: ClaudeEstimate }
    | { kind: "code-ready"; cli: Cli }
    | { kind: "running"; code: boolean }
    | { kind: "done"; result: ClaudePlanResult }
    | { kind: "error"; message: string }
  >({ kind: "estimating" });

  // What the user sees first depends on the route: the exact price (API), or whether Claude Code is ready.
  useEffect(() => {
    setStep({ kind: "estimating" });
    if (props.method === "api") {
      invoke<ClaudeEstimate>("claude_estimate", { projectDir: props.projectDir })
        .then((est) => setStep({ kind: "ready", est }))
        .catch((e) => setStep({ kind: "error", message: String(e) }));
    } else {
      invoke<Cli>("claude_code_status")
        .then((cli) => setStep({ kind: "code-ready", cli }))
        .catch((e) => setStep({ kind: "error", message: String(e) }));
    }
  }, [props.projectDir, props.method]);

  async function runCode() {
    setStep({ kind: "running", code: true });
    try {
      const result = await invoke<ClaudePlanResult>("claude_code_plan", { projectDir: props.projectDir });
      if (result.outcome.applied && result.captions) props.onApplied(result.captions);
      setStep({ kind: "done", result });
    } catch (e) {
      setStep({ kind: "error", message: String(e) });
    }
  }

  async function run(est: ClaudeEstimate) {
    setStep({ kind: "running", code: false });
    try {
      const result = await invoke<ClaudePlanResult>("claude_plan", { projectDir: props.projectDir, confirmedUsd: est.worstCaseUsd });
      if (result.outcome.applied && result.captions) props.onApplied(result.captions);
      setStep({ kind: "done", result });
    } catch (e) {
      setStep({ kind: "error", message: String(e) });
    }
  }

  const busy = step.kind === "estimating" || step.kind === "running";
  return (
    <Modal
      title="Plan this edit with Claude"
      onClose={busy ? undefined : props.onClose}
      footer={
        step.kind === "code-ready" ? (
          <>
            <button className="btn" onClick={props.onClose}>Cancel</button>
            <button className="btn primary" disabled={!step.cli.installed || !step.cli.loggedIn} onClick={runCode}>Let Claude Code plan it</button>
          </>
        ) : step.kind === "running" && step.code ? (
          <button className="btn danger" onClick={() => invoke("claude_code_cancel")}>Stop Claude Code</button>
        ) : step.kind === "ready" ? (
          <>
            <button className="btn" onClick={props.onClose}>Cancel</button>
            <button className="btn primary" disabled={!step.est.allowed} onClick={() => run(step.est)}>
              Send to Claude (up to {usd(step.est.worstCaseUsd)})
            </button>
          </>
        ) : !busy ? (
          <button className="btn primary" onClick={props.onClose}>Close</button>
        ) : undefined
      }
    >
      <Segmented
        label="How to reach Claude"
        value={props.method}
        options={[
          { value: "code", label: "Claude Code (your subscription)" },
          { value: "api", label: "API key (pay per use)" },
        ]}
        onChange={(m) => !busy && props.onMethod(m)}
      />
      {props.method === "api" ? (
        <div className="callout warn"><p>This uses <strong>your own Claude API key</strong> and <strong>costs money</strong>. Nothing is sent until you press the button.</p></div>
      ) : (
        <div className="callout"><p>This runs <strong>Claude Code</strong> on this Mac with <strong>your Claude subscription</strong>, so there is no API charge, but it uses your plan's usage limits and can take a few minutes. It can only read and write inside this edit's folder. Nothing runs until you press the button.</p></div>
      )}
      {step.kind === "estimating" && <p className="muted">{props.method === "api" ? "Counting what would be sent (free)…" : "Checking Claude Code…"}</p>}
      {step.kind === "code-ready" && (
        <dl>
          <dt>Claude Code</dt><dd>{step.cli.installed ? `installed (${step.cli.version ?? "version unknown"})` : "not installed"}</dd>
          <dt>Login</dt><dd>{step.cli.loggedIn ? "logged in" : "not logged in"}</dd>
          <dt>Status</dt><dd className={step.cli.installed && step.cli.loggedIn ? "success" : "error-text"}>{step.cli.message}</dd>
        </dl>
      )}
      {step.kind === "ready" && (
        <dl>
          <dt>Model</dt><dd>{step.est.model}</dd>
          <dt>Sends</dt><dd>{step.est.inputTokens.toLocaleString()} tokens (the transcript and rules, no video)</dd>
          <dt>Worst-case cost</dt><dd><strong>{usd(step.est.worstCaseUsd)}</strong> for this one call</dd>
          <dt>Used so far</dt><dd>{usd(step.est.spentUsd)} over {step.est.callsMade} call(s); limit {usd(step.est.maxUsd)} and {step.est.maxCalls} calls</dd>
          {!step.est.allowed && <><dt>Blocked</dt><dd className="error-text">{step.est.blockedReason}</dd></>}
        </dl>
      )}
      {step.kind === "running" && <><p className="muted">{step.code ? "Claude Code is reading the transcript and writing a plan. This can take a few minutes…" : "Waiting for Claude…"}</p><progress /></>}
      {step.kind === "done" && (
        step.result.outcome.applied ? (
          <>
            <p className="success">Claude's plan was checked and applied.</p>
            <p className="note">{step.result.outcome.segments} segments, {step.result.outcome.captions} captions. {props.method === "api" ? `It cost ${usd(step.result.outcome.costUsd)}.` : "No API charge (it used your subscription)."} The captions list was updated; you can undo this from the caption check.</p>
          </>
        ) : (
          <>
            <p className="error-text">Claude's plan did not pass the app's checks, so it was not used. Your current plan is unchanged.</p>
            <pre className="error-text">{step.result.outcome.errors.slice(0, 6).join("\n")}</pre>
            {props.method === "api" && <p className="note">This call cost {usd(step.result.outcome.costUsd)}.</p>}
          </>
        )
      )}
      {step.kind === "error" && <pre className="error-text" role="alert">{step.message}</pre>}
    </Modal>
  );
}

type KeyStatus = { source: "env" | "keychain" | "none"; account: string };
type Fix = { from: string; to: string };

export function SettingsDialog(props: {
  kitDir: string | null;
  onKitDir: (dir: string | null) => void;
  useKit: boolean;
  useClaude: boolean;
  onUseKit: (on: boolean) => void;
  onUseClaude: (on: boolean) => void;
  engine: EngineStatus | null;
  onClose: () => void;
}) {
  const [keyStatus, setKeyStatus] = useState<KeyStatus | null>(null);
  const [keyInput, setKeyInput] = useState("");
  const [msg, setMsg] = useState<{ ok: boolean; text: string } | null>(null);
  const [saving, setSaving] = useState(false);
  const [fixes, setFixes] = useState<Fix[] | null>(null);
  const [dictMsg, setDictMsg] = useState<string | null>(null);
  const [kit, setKit] = useState<{ found: boolean; dir: string | null; message: string } | null>(null);

  useEffect(() => {
    invoke<{ found: boolean; dir: string | null; message: string }>("kit_status", { dir: props.kitDir }).then(setKit).catch(() => setKit(null));
  }, [props.kitDir]);

  async function chooseKit() {
    const picked = await open({ directory: true, multiple: false, title: "Choose the free247editor kit folder" });
    if (typeof picked === "string") props.onKitDir(picked);
  }

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
      <section className="settings-section" aria-label="Engines">
        <h3>Engines</h3>
        <p className="note">Two separate switches. Neither one needs the other, and any mix works. Nothing switches by itself: if something needs a switch that is off, it stops and tells you which one.</p>
        <Toggle
          label="Use herk-edit-kit"
          hint="Local. Draws animated graphics (HyperFrames) and builds the edit-review page. OFF: only the app's own engine is used."
          checked={props.useKit}
          onChange={props.onUseKit}
        />
        <div className="engine-detail" role="status">
          <span className={`dot ${!props.useKit ? "off" : props.engine?.kit.found && props.engine.kit.hyperframesVersion ? "ok" : "bad"}`} aria-hidden="true" />
          <div>
            <p><strong>{props.engine?.kit.message ?? "Checking…"}</strong></p>
            {props.engine?.kit.found && (
              <p className="note">
                {props.engine.kit.dir} · Node {props.engine.kit.nodeVersion ?? "?"} · HyperFrames {props.engine.kit.hyperframesVersion ?? "did not run"}
              </p>
            )}
            {props.engine?.kit.found && (
              <ul className="tool-list">
                {props.engine.kit.tools.map((t) => (
                  <li key={t.name} className={t.present ? "" : "missing"}>{t.present ? "✓" : "✗"} {t.name} <span className="muted">— {t.what}</span></li>
                ))}
              </ul>
            )}
          </div>
        </div>
        <Toggle
          label="Use Claude AI"
          hint="Online, and it costs money. Lets you ask Claude to plan an edit, after showing the price. OFF: no request is ever sent to Claude."
          checked={props.useClaude}
          onChange={props.onUseClaude}
        />
        <div className="engine-detail" role="status">
          <span className={`dot ${props.useClaude ? (props.engine?.claude.keySource === "none" ? "bad" : "ok") : "off"}`} aria-hidden="true" />
          <div>
            <p><strong>{props.engine?.claude.message ?? "Checking…"}</strong></p>
            {props.engine && (
              <p className="note">API key: {props.engine.claude.keySource === "none" ? "not set" : `found (${props.engine.claude.keySource})`} · used {usd(props.engine.claude.spentUsd)} over {props.engine.claude.callsMade} call(s); limit {usd(props.engine.claude.maxUsd)} / {props.engine.claude.maxCalls} calls</p>
            )}
          </div>
        </div>
        {props.useClaude && props.engine?.claudeCode && (
          <div className="engine-detail" role="status">
            <span className={`dot ${props.engine.claudeCode.installed && props.engine.claudeCode.loggedIn ? "ok" : "bad"}`} aria-hidden="true" />
            <div>
              <p><strong>Claude Code: {props.engine.claudeCode.installed ? (props.engine.claudeCode.loggedIn ? "ready" : "not logged in") : "not installed"}</strong></p>
              <p className="note">{props.engine.claudeCode.message}{props.engine.claudeCode.version ? ` (${props.engine.claudeCode.version})` : ""}</p>
            </div>
          </div>
        )}
        <p className="note">Works without either: opening, editing, previewing and exporting. Offline: cuts, captions, transitions, sounds, hook, thumbnails. Needs the kit: animated graphics, the edit-review page. Needs Claude and internet: the "Plan with Claude" button (Claude Code uses your subscription; the API key route is pay per use).</p>
      </section>

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
        <h3>Animated graphics (HyperFrames)</h3>
        <p className="note">Motion cards, stickers and arrows are drawn by HyperFrames, which comes with the free247editor kit. Choose its <strong>kit</strong> folder if it is not found automatically.</p>
        <div className="file-row">
          <span className={`file-name${kit?.found ? "" : " empty"}`} title={kit?.dir ?? ""}>{kit?.found ? kit.dir : "Not found"}</span>
          <button className="btn small" onClick={chooseKit}>Choose folder…</button>
          {props.kitDir && <button className="icon-btn" aria-label="Use the default folder" onClick={() => props.onKitDir(null)}>✕</button>}
        </div>
        <p className={kit?.found ? "success" : "error-text"}>{kit?.message ?? "Checking…"}</p>
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
