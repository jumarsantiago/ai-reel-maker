# AI Reel Maker — Day 1 completion report (2026-10-22)

Runbook: Notion "AI Reel Maker — One-Day MVP Sprint & Claude Code Runbook v0.1".
Project: `~/Projects/ai-video-edit` (you asked for this path; the runbook's default was `~/Projects/ai-reel-maker-day1`).

## Result

**All P0 items are done and verified.** This is a working macOS desktop prototype. It imports and previews real
video, lets you trim and reorder clips on a timeline, and exports a genuine MP4 through local FFmpeg. Every
export was checked independently: its streams, duration, full decode, and frame matching for order and trims.
**P1 items were not started.** Those are aspect-ratio presets, project save/reopen, text overlays and UI polish.

## Acceptance checklist (runbook §6)

| # | Criterion | Status | Evidence |
|---|---|---|---|
| 1 | Desktop app launches on the Mac | ✅ | Debug and release `.app` both launched; screenshots of the dark workspace |
| 2 | Import and preview a real local video | ✅ | You checked this by hand with `clip-a.mp4` and your own recording; I imported through the native picker (M2–M5) |
| 3 | Timeline reorder/trim values are validated and used by rendering | ✅ | 8 timeline unit tests; the backend re-validates against ffprobe; the in-app render plan matched every export |
| 4 | The app's rendering path creates a genuine MP4 via FFmpeg | ✅ | 3 exports made through the app's UI (M3, M4, M5 release build) |
| 5 | Output passes technical inspection and plays | ✅ | `verify_export.py` passed 8/8, 10/10 and 8/8; QuickTime played the M4 file in real time through to 6.0s |
| 6 | At least one end-to-end export test is run and reported | ✅ | `real_export_end_to_end` (automated) plus three UI exports, all in WORKLOG.md |
| 7 | Source media remains unchanged | ✅ | `test-media/SHA256SUMS` matched after every export; byte comparison in the Rust test |
| 8 | Build/test commands and limitations are documented | ✅ | README.md |
| 9 | No extra spending or paid API calls | ✅ | Only free tools: rustup, Homebrew FFmpeg, npm packages. No APIs, no uploads |
| 10 | Personal Kailan Lang files not changed | ✅ | None accessed; `~/Projects` contains only this project. The Kailan Lang Notion page was only read |

## Test results (final run)

- `npm test`: 8 passed, 0 failed
- `npm run build`: OK
- `cd src-tauri && cargo test --lib`: 13 passed, 0 failed, including the real FFmpeg end-to-end export
- `npm run tauri build -- --bundles app`: OK in 1m 07s, producing a 4.3 MB unsigned `AI Reel Maker.app`
- Release-build UI export, `exports/m5-release.mp4` (clip-c 0→6, clip-a 2→4): 8/8 checks passed;
  8.000s, H.264 1280×720 30 fps, AAC 48 kHz stereo

## Key decisions

- **Safety:** FFmpeg and ffprobe run with separate arguments, never through a shell, and only video file types on an allowlist are accepted.
- **Preview access:** the preview can read only the files you import, not the rest of the disk.
- **Validation:** the backend re-checks every trim. It renders to a temp file, inspects it, then renames it into place.
- **Overwrites:** existing files are never silently replaced, and source clips can never be overwritten.
- **Output format:** the first clip's resolution and frame rate. Other clips are letterboxed, and clips without audio get silence.
- **Dependencies:** no test framework was added. The frontend uses Node's built-in test runner.

## Known limitations and risks

- Unsigned and not notarized; macOS only. There's no installer or Windows build.
- No cancel during export, no project save, no undo, no full-timeline playback, no aspect-ratio presets.
- The Content Security Policy is disabled for the prototype.
- **Licensing:** the Homebrew FFmpeg is a GPL build with libx264. It needs a review before any commercial distribution.
- Tested only on H.264/AAC test patterns and one real H.264 recording.
- Testing in the UI needed the full-screen control you approved. Without it, macOS limits clicking, typing and the file picker.

## Housekeeping for you

- Leftovers from an earlier, out-of-plan session are still in the folder: `_to_delete/_failed_clone_kit/` (an empty
  failed git clone) and `WORKLOG.old-herk.md`. You can safely delete them; I didn't.
- `exports/` holds the three test exports and is ignored by git.
- The project isn't a git repository yet.

## Suggested next milestone

After P1, the runbook's next step is automatic editing. Free, local options exist: FFmpeg silence detection for
cutting pauses, FFmpeg scene detection for splitting long footage, and a local Whisper model for captions.
Each would generate timeline clips, which the verified render pipeline already exports.
