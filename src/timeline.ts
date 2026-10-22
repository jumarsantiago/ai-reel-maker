// Timeline model: the single source of the render instructions M3 will execute.
// Pure functions only, so they can be unit-tested without the UI.

export const MIN_CLIP_SEC = 0.1;

export type TimelineClip = {
  id: string;
  sourcePath: string;
  fileName: string;
  sourceDuration: number;
  inPoint: number;
  outPoint: number;
};

export type RenderSegment = {
  order: number;
  sourcePath: string;
  inPoint: number;
  outPoint: number;
};

let nextId = 1;

export function createClip(source: { path: string; fileName: string; durationSec: number }): TimelineClip {
  return {
    id: `clip-${nextId++}`,
    sourcePath: source.path,
    fileName: source.fileName,
    sourceDuration: source.durationSec,
    inPoint: 0,
    outPoint: source.durationSec,
  };
}

export function clipLength(clip: TimelineClip): number {
  return clip.outPoint - clip.inPoint;
}

/** Returns an error message, or null when the trim range is valid for the clip's source. */
export function validateTrim(sourceDuration: number, inPoint: number, outPoint: number): string | null {
  if (!Number.isFinite(inPoint) || !Number.isFinite(outPoint)) return "In and out must be numbers.";
  if (inPoint < 0) return "In point cannot be before 0.";
  if (outPoint > sourceDuration + 1e-6) return `Out point cannot be after the clip end (${sourceDuration.toFixed(2)}s).`;
  if (outPoint - inPoint < MIN_CLIP_SEC) return `Out must be at least ${MIN_CLIP_SEC}s after in.`;
  return null;
}

export type TrimResult = { ok: true; clip: TimelineClip } | { ok: false; error: string };

export function setTrim(clip: TimelineClip, inPoint: number, outPoint: number): TrimResult {
  const error = validateTrim(clip.sourceDuration, inPoint, outPoint);
  if (error) return { ok: false, error };
  return { ok: true, clip: { ...clip, inPoint, outPoint: Math.min(outPoint, clip.sourceDuration) } };
}

/** Moves the clip at `index` by `delta` positions; out-of-range moves return the same array. */
export function moveClip(clips: TimelineClip[], index: number, delta: number): TimelineClip[] {
  const target = index + delta;
  if (index < 0 || index >= clips.length || target < 0 || target >= clips.length) return clips;
  const next = [...clips];
  const [moved] = next.splice(index, 1);
  next.splice(target, 0, moved);
  return next;
}

export function removeClip(clips: TimelineClip[], id: string): TimelineClip[] {
  return clips.filter((c) => c.id !== id);
}

export function totalDuration(clips: TimelineClip[]): number {
  return clips.reduce((sum, c) => sum + clipLength(c), 0);
}

/** Converts timeline state into ordered render segments, rejecting any invalid clip. */
export function toRenderPlan(clips: TimelineClip[]): { ok: true; segments: RenderSegment[] } | { ok: false; error: string } {
  if (clips.length === 0) return { ok: false, error: "Timeline is empty." };
  const segments: RenderSegment[] = [];
  for (const [i, c] of clips.entries()) {
    const error = validateTrim(c.sourceDuration, c.inPoint, c.outPoint);
    if (error) return { ok: false, error: `Clip ${i + 1} (${c.fileName}): ${error}` };
    segments.push({ order: i + 1, sourcePath: c.sourcePath, inPoint: c.inPoint, outPoint: c.outPoint });
  }
  return { ok: true, segments };
}
