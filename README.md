# AI Reel Maker — Day 1 prototype

A minimal macOS desktop video editor: import local videos, put clips on a timeline, trim and reorder them,
and export a real MP4 with your local FFmpeg. Built with Tauri 2, React and TypeScript, and a Rust backend.

> **Status:** local development prototype (Day 1 sprint). It's unsigned and not distributable.
> There are no AI features yet. See [Known limitations](#known-limitations).

## Prerequisites (macOS, Apple Silicon)

| Tool | Version verified | Install |
|---|---|---|
| Xcode / Command Line Tools | Xcode 26.6 | App Store, or `xcode-select --install` |
| Node.js + npm | Node 24.14.1, npm 11.11.0 | <https://nodejs.org> |
| Rust + Cargo | 1.99.1 | `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \| sh` |
| FFmpeg + FFprobe | 9.0.2 | `brew install ffmpeg` |

The app looks for `ffmpeg`/`ffprobe` in `/opt/homebrew/bin`, then `/usr/local/bin`, then `/usr/bin`, then `PATH`.
It does this so the app still works when it's launched from Finder.

## Install

```bash
npm install
```

## Run

Development mode, with hot reload for the UI:

```bash
npm run tauri dev
```

Build a local app bundle. This is unsigned and only for this Mac:

```bash
npm run tauri build -- --bundles app
```

That produces `src-tauri/target/release/bundle/macos/AI Reel Maker.app`. A faster debug build is
`npm run tauri build -- --debug --bundles app`, which puts the app under `src-tauri/target/debug/bundle/macos/`.

## Test

| Command | What it covers |
|---|---|
| `npm test` | Timeline model: trim validation, reorder, remove, render-plan order (Node's built-in test runner) |
| `npm run build` | TypeScript type-check and production frontend build |
| `cd src-tauri && cargo test --lib` | ffprobe parsing and path checks, FFmpeg argument and filter-graph building, output-path safety, plus a **real end-to-end export** (runs only if FFmpeg is installed; skipped otherwise) |
| `python3 -I scripts/verify_export.py OUT.mp4 SRC:IN:OUT ...` | Checks an exported file on its own: H.264 stream, duration, full decode, and frame matching (PSNR) at each segment's start and end, which proves both the order and the trim points |

Example, for a timeline of `clip-b` (0–4s) followed by `clip-a` trimmed to 1–3s:

```bash
python3 -I scripts/verify_export.py exports/out.mp4 test-media/clip-b.mp4:0:4 test-media/clip-a.mp4:1:3
```

Test clips live in `test-media/`. They're synthetic FFmpeg test patterns with tones, and their checksums are in
`test-media/SHA256SUMS`. To confirm the sources were never modified:

```bash
cd test-media && shasum -a 256 -c SHA256SUMS
```

## Export workflow

1. **Import:** choose one or more videos (`mp4`, `mov`, `m4v`, `webm`, `mkv`). FFprobe reads each file's
   duration, resolution, codec, frame rate and audio, and the properties panel shows them.
2. **Add to timeline:** click **+** next to a media item. The same video can be added more than once.
3. **Trim:** select a timeline clip, then set **In / Out** and click **Apply trim**, or use **Set in/out at playhead**.
   Invalid ranges are rejected with a message: before 0, past the end, reversed, or shorter than 0.1s.
4. **Reorder or remove:** use **◀ Earlier**, **Later ▶** and **Remove**.
5. **Check the render plan:** the line under the timeline shows exactly what export will render.
6. **Export:** click **Export**, choose a file name and folder, and watch the progress bar.
   When it finishes, the result shows the duration, resolution, audio and size, plus **Show in Finder**.

How export works:

- The backend re-reads every source with ffprobe and re-checks every trim. It doesn't trust the UI's values.
- It runs **one FFmpeg process with separate arguments, never through a shell**.
- Every clip is scaled and letterboxed to the first clip's resolution and frame rate.
- Clips without audio get silence, so the audio stays in sync.
- The output is H.264 (CRF 20) + AAC 160 kbps, with `+faststart`.
- It renders to a hidden `.<name>.partial-*.mp4` next to the output, checks the result, and only then
  renames it into place. A failed export never leaves a half-written file.
- It never writes over a source clip. It only replaces an existing file when you confirmed that in the macOS save dialog.
- Your original media is only read, never modified.

## Auto Edit (free, no AI)

In the app, select a video in the media bin and click **✨ Auto Edit (free, no AI)**. Everything runs on this Mac.

1. **Transcribe** the speech with whisper.cpp.
2. **Cut** pauses longer than 0.4s, fillers (um/uh/ah/er/hmm) and repeated or restarted phrases. The last attempt is kept.
3. **Style** the reel. Each option can be switched on or off in the panel:
   - **Zoom on cuts:** every other segment is punched in to 115%, which hides jump cuts.
   - **Whoosh on cuts:** a soft whoosh just before each cut. The app generates the sound itself with FFmpeg, so there's no licensing issue.
   - **Highlight spoken word:** big captions of 1–3 words, with the word being spoken shown in yellow.
   - **Even out volume:** the final mix is normalized to −14 LUFS, with true peak at or below −1.5 dB.
   - **Music:** optional. Choose a file you have the rights to. It's looped, and ducked by about 5 dB under speech while sitting more than 20 dB below the voice.
4. **Render** a 1080×1920 MP4, then **check** it by re-transcribing the result.

Results go to `~/Movies/AI Reel Maker/<time>-<clip>/output.mp4`, along with `style.json`, the plan and the logs.
**Add to media & preview** loads the reel back into the app.

Auto Edit never cuts the Tagalog words "ano" or "parang", because they're often real words. It never reorders lines or
picks a hook; that's what the Claude planner (below) adds. CLI equivalent (style flags: `--plain --no-zoom --no-sfx
--no-highlight --no-loudness --music FILE`):

```bash
src-tauri/target/release/autoedit autocut test-media/clip-speech.mp4 --music test-media/test-music.m4a
```

Verify a styled reel (zoom-aware frame matching, caption visibility, loudness):

```bash
python3 -I scripts/verify_export.py "<project>/output.mp4" --validated "<project>/plan.validated.json" \
  --source test-media/clip-speech.mp4 --fit crop --srt "<project>/output.srt" --style "<project>/style.json" --lufs -14
```

The **Settings** window (top bar) stores a Claude API key in the macOS login Keychain. Only the Claude planner needs it,
and that is billed separately from Claude Pro.

## Auto-edit pipeline (Option B, in progress)

These local steps need `brew install whisper-cpp` and the multilingual Whisper model at
`~/Library/Application Support/ai-reel-maker/models/ggml-small.bin` (or set `AIRM_WHISPER_MODEL`).
**No network calls happen in these steps.** The Claude planning call is a separate, approved step (M7).

```bash
cd src-tauri && cargo build --release --bin autoedit && cd ..
src-tauri/target/release/autoedit prepare test-media/clip-speech.mp4 --lang auto
src-tauri/target/release/autoedit render projects/<id>-clip-speech test-media/clip-speech.fixture-plan.json
python3 -I scripts/verify_export.py projects/<id>-clip-speech/output.mp4 \
  --validated projects/<id>-clip-speech/plan.validated.json --source test-media/clip-speech.mp4 \
  --fit crop --srt projects/<id>-clip-speech/output.srt
```

- **`prepare`** extracts the audio and finds silences (FFmpeg). It transcribes each speech chunk locally with whisper.cpp
  and places word edges using DTW anchors and audio energy. It then writes `transcript.json` and **`request.json`**,
  the exact Messages API body that would be sent: transcript and metadata only, with no media or paths. Nothing is sent.
- **`render`** validates a plan and writes `plan.validated.json`, or `plan.errors.json` if it's rejected.
  It draws the caption PNGs and `output.srt`, then renders a 1080×1920 `output.mp4` (centre-cropped).
  Finally it re-transcribes the output and records how many of the kept words it heard (`render_report.json`).
- **Project folders** are `projects/<unix-time>-<clip>/`, which git ignores. Each step is logged in `log.txt`.
- The design, data-sharing and cost proposal is in `docs/OPTION_B_PLAN.md`.

## Project layout

```
src/                 React UI (App.tsx), timeline model (timeline.ts) + tests
src-tauri/src/       Rust backend: media.rs (ffprobe), render.rs (export), lib.rs (Tauri commands),
                     transcribe.rs, plan.rs (schema/request/validator), autoplan.rs (Auto-cut rules), api.rs,
                     captions.rs, autoedit.rs, bin/autoedit.rs
scripts/             verify_export.py (independent output inspection)
test-media/          synthetic test clips + SHA256SUMS
exports/             default place for test exports (git-ignored)
WORKLOG.md           milestone log with verified commands and results
```

## Known limitations

- **macOS only and unsigned:** no notarization, no installer, no Windows build.
- **No cancel button** during export, and nothing can be edited while it renders.
- **Output size and frame rate follow the first clip.** There are no 9:16 / 16:9 presets yet (a later P1 item).
- **Always re-encodes:** there's no fast "copy" path for clips that don't need it.
- **No project save or reopen, no text overlays, no undo.**
- **Clip-level previews only:** you preview one clip (confined to its trim), not the whole timeline played back.
- **AI auto-edit is partly built.** Local transcription, plan validation and 9:16 caption rendering work via the CLI. The Claude call (M7) and the UI button (M8) are not done yet.
- **Content Security Policy is disabled** (`csp: null`) for the prototype. The asset protocol starts with an
  empty scope and only lets the preview read the files you import.
- **Tested on:** macOS 27.0.1 (arm64), with H.264/AAC MP4 test patterns and one real H.264 recording.
  Other codecs, very long files and variable frame rate footage haven't been tested.

## Licensing note (read before selling or distributing)

The Homebrew FFmpeg used here is built with `--enable-gpl --enable-libx264`, which makes it **GPL**.
This prototype calls the system `ffmpeg` as a separate program and doesn't bundle it. A commercial release
would need a licensing review of FFmpeg, x264 and AAC, plus every npm and Cargo dependency. One option is
an LGPL-only FFmpeg build using the macOS VideoToolbox encoder instead of x264. A local prototype isn't
automatically cleared for sale.
