#!/usr/bin/env python3
"""Independently inspect an exported MP4 against the timeline that produced it.

Usage:
  python3 -I scripts/verify_export.py OUTPUT.mp4 SOURCE:IN:OUT [SOURCE:IN:OUT ...]
  python3 -I scripts/verify_export.py OUTPUT.mp4 --validated plan.validated.json --source SRC.mp4 \
      [--fit crop] [--srt output.srt]

--fit crop   compare against the source scaled-to-cover and centre-cropped (vertical reels).
--srt FILE   frame matching then uses only the area above the caption band, and each caption is
             checked to be visible: its band must differ clearly from the uncaptioned source.
--style FILE style.json from the render: applies each segment's punch-in zoom when matching frames.
--lufs N     integrated loudness must be within 1 LU of N and true peak at most -1 dBTP.

Checks: file exists and is non-empty; ffprobe finds an H.264 video stream; total duration matches the
sum of segments; every frame decodes; and for each segment, frames near its start and end match the
expected source frames (PSNR), which confirms both order and trim points. Exit code 0 = all passed.
Uses only ffmpeg/ffprobe with argument lists (no shell).
"""
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile

DURATION_TOL = 0.15  # seconds
PSNR_MIN = 30.0      # dB; a wrong frame or wrong clip scores far lower on the test patterns


def tool(name):
    for d in ("/opt/homebrew/bin", "/usr/local/bin"):
        p = os.path.join(d, name)
        if os.path.isfile(p):
            return p
    p = shutil.which(name)
    if not p:
        sys.exit(f"{name} not found")
    return p


FFMPEG, FFPROBE = tool("ffmpeg"), tool("ffprobe")


def probe(path):
    out = subprocess.run(
        [FFPROBE, "-v", "error", "-print_format", "json", "-show_format", "-show_streams", path],
        capture_output=True, text=True, check=True,
    ).stdout
    return json.loads(out)


CAPTION_BAND = (0.60, 0.80)  # fraction of height; captions are centred at 0.70


def psnr(output, t_out, source, t_src, w, h, fit_mode="pad", band=None, zoom=1.0):
    if fit_mode == "crop":
        fit = f"scale={w}:{h}:force_original_aspect_ratio=increase,crop={w}:{h}"
        if zoom > 1.001:
            zw, zh = (round(w * zoom) // 2) * 2, (round(h * zoom) // 2) * 2
            fit += f",scale={zw}:{zh},crop={w}:{h}"
        fit += ",setsar=1"
    else:
        fit = f"scale={w}:{h}:force_original_aspect_ratio=decrease,pad={w}:{h}:(ow-iw)/2:(oh-ih)/2,setsar=1"
    roi = ""
    if band is not None:  # (y0, y1) as fractions of height
        y0, y1 = int(band[0] * h) // 2 * 2, int(band[1] * h) // 2 * 2
        roi = f",crop={w}:{y1 - y0}:0:{y0}"
    # Per-frame stats go to a file: this FFmpeg build sometimes drops the psnr summary log line.
    fd, stats = tempfile.mkstemp(suffix=".psnr.log")
    os.close(fd)
    try:
        subprocess.run(
            # Inputs are limited to ~1 frame with -t so the graph ends at natural EOF (with
            # -frames:v the filter is sometimes torn down before it flushes its stats).
            [FFMPEG, "-hide_banner", "-nostdin", "-ss", f"{t_out:.4f}", "-t", "0.02", "-i", output,
             "-ss", f"{t_src:.4f}", "-t", "0.02", "-i", source,
             "-filter_complex",
             f"[1:v]{fit}{roi},format=yuv420p[s];[0:v]setsar=1{roi},format=yuv420p[o];[o][s]psnr=stats_file={stats}",
             "-an", "-f", "null", "-"],
            capture_output=True, text=True,
        )
        m = re.search(r"psnr_avg:(inf|[\d.]+)", open(stats).read())
    finally:
        os.remove(stats)
    if not m:
        return None
    return float("inf") if m.group(1) == "inf" else float(m.group(1))


def parse_srt(path):
    caps = []
    for block in open(path, encoding="utf-8").read().strip().split("\n\n"):
        lines = block.strip().splitlines()
        if len(lines) < 3:
            continue
        to_s = lambda x: (lambda h, m, s: int(h) * 3600 + int(m) * 60 + float(s.replace(",", ".")))(*x.strip().split(":"))
        a, b = lines[1].split("-->")
        caps.append((to_s(a), to_s(b), " ".join(lines[2:])))
    return caps


def main():
    args = sys.argv[1:]
    def take(flag):
        if flag in args:
            i = args.index(flag)
            value = args[i + 1]
            del args[i:i + 2]
            return value
        return None
    fit_mode = take("--fit") or "pad"
    srt = take("--srt")
    validated = take("--validated")
    style_file = take("--style")
    lufs = take("--lufs")
    source = take("--source")
    if not args or (validated is None and len(args) < 2) or fit_mode not in ("pad", "crop"):
        sys.exit(__doc__)
    output = args[0]
    segments = []
    if validated:
        if not source:
            sys.exit("--validated needs --source")
        for seg in json.load(open(validated))["segments"]:
            segments.append((source, float(seg["start"]), float(seg["end"])))
    for spec in args[1:]:
        src, a, b = spec.rsplit(":", 2)
        segments.append((src, float(a), float(b)))
    captions = parse_srt(srt) if srt else []
    zooms = json.load(open(style_file))["zoom_per_segment"] if style_file else []
    zoom_of = lambda i: zooms[i] if i < len(zooms) else 1.0
    # With captions, frame matching uses only the area above the caption band.
    match_band = (0.0, CAPTION_BAND[0]) if captions else None

    results = []

    def check(name, ok, detail):
        results.append(ok)
        print(f"[{'PASS' if ok else 'FAIL'}] {name}: {detail}")

    size = os.path.getsize(output) if os.path.isfile(output) else 0
    check("file", size > 0, f"{size} bytes")
    if size == 0:
        return 1

    info = probe(output)
    video = next((s for s in info["streams"] if s["codec_type"] == "video"), None)
    audio = next((s for s in info["streams"] if s["codec_type"] == "audio"), None)
    check("video stream", video is not None and video.get("codec_name") == "h264",
          f"{video.get('codec_name')} {video.get('width')}x{video.get('height')} {video.get('r_frame_rate')} "
          f"{video.get('pix_fmt')}" if video else "missing")
    if not video:
        return 1
    print(f"[INFO] audio stream: {audio['codec_name']} {audio.get('sample_rate')} Hz {audio.get('channels')} ch"
          if audio else "[INFO] audio stream: none")
    w, h = video["width"], video["height"]
    num, den = (int(x) for x in video["r_frame_rate"].split("/"))
    fps = num / den

    expected = sum(b - a for _, a, b in segments)
    actual = float(info["format"]["duration"])
    # Each segment is rounded up to whole frames (concat pads its audio to match, so A/V stay in sync).
    tol = DURATION_TOL + len(segments) / fps
    check("duration", abs(actual - expected) <= tol, f"{actual:.3f}s (expected {expected:.3f}s, tolerance {tol:.2f}s)")

    dec = subprocess.run([FFMPEG, "-v", "error", "-nostdin", "-i", output, "-f", "null", "-"],
                         capture_output=True, text=True)
    check("full decode", dec.returncode == 0 and not dec.stderr.strip(),
          "no errors" if dec.returncode == 0 and not dec.stderr.strip() else dec.stderr.strip()[-300:])

    # Sample mid-frame times (avoid frame-boundary rounding) near each segment's start and end.
    offset = 3.5 / fps
    start = 0.0
    for i, (src, a, b) in enumerate(segments, 1):
        length = b - a
        for label, rel in (("start", offset), ("end", length - offset)):
            p = psnr(output, start + rel, src, a + rel, w, h, fit_mode, match_band, zoom_of(i - 1))
            check(f"segment {i} {label}", p is not None and p >= PSNR_MIN,
                  f"out@{start + rel:.3f}s vs {os.path.basename(src)}@{a + rel:.3f}s PSNR "
                  f"{'n/a' if p is None else ('inf' if p == float('inf') else f'{p:.1f} dB')}")
        start += length

    # Captions: at each caption's midpoint the caption band must differ clearly from the source
    # (text drawn there), while the area above it still matches (same frame, correct timing).
    if captions:
        bounds, t = [], 0.0
        for k, (src, a, b) in enumerate(segments):
            bounds.append((t, t + (b - a), src, a, zoom_of(k)))
            t += b - a
        for i, (ca, cb, text) in enumerate(captions, 1):
            mid = (ca + cb) / 2
            seg = next((x for x in bounds if x[0] <= mid < x[1]), None)
            if seg is None:
                check(f"caption {i}", False, f"{text!r} at {mid:.2f}s is outside every segment")
                continue
            src_t = seg[3] + (mid - seg[0])
            band = psnr(output, mid, seg[2], src_t, w, h, fit_mode, CAPTION_BAND, seg[4])
            above = psnr(output, mid, seg[2], src_t, w, h, fit_mode, match_band, seg[4])
            ok = band is not None and above is not None and above >= PSNR_MIN and band <= above - 8
            check(f"caption {i}", ok, f"{text!r} @{mid:.2f}s band {band if band is None else round(band, 1)} dB "
                  f"vs above {above if above is None else round(above, 1)} dB")

    if lufs is not None:
        r = subprocess.run([FFMPEG, "-hide_banner", "-nostdin", "-nostats", "-i", output, "-vn", "-af", "ebur128=peak=true",
                            "-f", "null", "-"], capture_output=True, text=True)
        i_match = re.findall(r"I:\s+(-?[\d.]+) LUFS", r.stderr)
        p_match = re.findall(r"Peak:\s+(-?[\d.]+|-inf) dBFS", r.stderr)
        integrated = float(i_match[-1]) if i_match else None
        peak = float(p_match[-1]) if p_match and p_match[-1] != "-inf" else None
        check("loudness", integrated is not None and abs(integrated - float(lufs)) <= 1.0 and peak is not None and peak <= -1.0,
              f"{integrated} LUFS (target {lufs}), true peak {peak} dBTP")

    passed = sum(results)
    print(f"\n{passed}/{len(results)} checks passed")
    return 0 if all(results) else 1


if __name__ == "__main__":
    sys.exit(main())
