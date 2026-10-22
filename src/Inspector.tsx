// The right-hand panel: how the reel should look and sound, and the button that makes it.
import { useState } from "react";
import { CaptionTemplate, ExtrasDraft, Settings } from "./settings.ts";
import { Section, Segmented, SelectField, Slider, Tabs, Toggle } from "./ui.tsx";

export type FontInfo = { name: string; path: string };

const CAPTION_TEMPLATES: { value: CaptionTemplate; label: string }[] = [
  { value: "soft", label: "Soft plum" },
  { value: "clean", label: "Clean — no background" },
  { value: "blush", label: "Blush pink" },
  { value: "dark", label: "Dark box" },
  { value: "classic", label: "Classic outline" },
];

type Tab = "style" | "audio" | "text" | "output";

export function Inspector(props: {
  settings: Settings;
  update: (patch: Partial<Settings>) => void;
  fonts: FontInfo[];
  fontPreview: string | null;
  extras: ExtrasDraft;
  setExtras: (d: ExtrasDraft) => void;
  chooseMusic: () => void;
  /** What the main button will work on, e.g. "3 timeline clips" or "mouse.mp4"; null when nothing is ready. */
  target: string | null;
  /** Why the main button is disabled (shown under it), if it is. */
  blocker: string | null;
  running: boolean;
  onRun: () => void;
  onPreview: () => void;
}) {
  const { settings: s, update } = props;
  const [tab, setTab] = useState<Tab>("style");
  const travel = s.mode === "travel";

  const style = (
    <>
      <Section title="Shape">
        <Segmented
          label="Video shape"
          value={s.orientation}
          options={[
            { value: "vertical", label: "Vertical 9:16" },
            { value: "horizontal", label: "Horizontal 16:9" },
          ]}
          onChange={(orientation) => update({ orientation })}
        />
        <SelectField
          label="Clips of a different shape"
          value={s.fitBlur ? "blur" : "fill"}
          options={[
            { value: "fill", label: "Fill the frame (crop the edges)" },
            { value: "blur", label: "Whole picture on a blurred background" },
          ]}
          onChange={(v) => update({ fitBlur: v === "blur" })}
        />
      </Section>

      <Section title={travel ? "Titles" : "Captions"}>
        {!travel && (
          <SelectField
            label="Caption style"
            value={s.captionTemplate}
            options={CAPTION_TEMPLATES}
            onChange={(captionTemplate) => update({ captionTemplate })}
          />
        )}
        <SelectField
          label="Font"
          value={s.captionFont ?? ""}
          options={[{ value: "", label: "Default (rounded)" }, ...props.fonts.map((f) => ({ value: f.path, label: f.name }))]}
          onChange={(v) => update({ captionFont: v || null })}
        />
        {props.fontPreview && (
          <div className="caption-preview">
            <img src={props.fontPreview} alt="How the captions will look" />
          </div>
        )}
        {!travel && (
          <Toggle label="Highlight the spoken word" checked={s.highlightWords} onChange={(highlightWords) => update({ highlightWords })} />
        )}
      </Section>

      {travel ? (
        <Section title="Clips">
          <SelectField
            label="Longest per clip"
            value={s.clipSec}
            options={[
              { value: 2.5, label: "2.5 s — fast" },
              { value: 3, label: "3 s" },
              { value: 4, label: "4 s" },
              { value: 5, label: "5 s" },
              { value: 6, label: "6 s" },
              { value: 0, label: "Keep my trims" },
            ]}
            onChange={(clipSec) => update({ clipSec })}
          />
          <SelectField
            label="Transition"
            value={s.transition}
            options={[
              { value: "smooth", label: "Smooth fade" },
              { value: "dynamic", label: "Dynamic mix (slides, zooms)" },
              { value: "none", label: "Hard cut" },
            ]}
            onChange={(transition) => update({ transition })}
          />
          <SelectField
            label="Colour look"
            value={s.grade}
            options={[
              { value: "warm", label: "Warm travel" },
              { value: "vivid", label: "Vivid" },
              { value: "natural", label: "Natural" },
            ]}
            onChange={(grade) => update({ grade })}
          />
          <Toggle label="Pick the best part of each clip" hint="Skips blurry and shaky moments" checked={s.smartPick} onChange={(smartPick) => update({ smartPick })} />
          <Toggle label="Slow drift on each clip" checked={s.pan} onChange={(pan) => update({ pan })} />
          <Toggle label="Fade in and out" checked={s.fade} onChange={(fade) => update({ fade })} />
        </Section>
      ) : (
        <Section title="Editing">
          <Toggle label="Zoom on cuts" hint="Hides jump cuts" checked={s.zoom} onChange={(zoom) => update({ zoom })} />
          <Toggle label="Whoosh on cuts" checked={s.soundEffects} onChange={(soundEffects) => update({ soundEffects })} />
        </Section>
      )}
    </>
  );

  const audio = (
    <>
      <Section title="Music">
        <div className="file-row">
          <span className={`file-name${s.music ? "" : " empty"}`} title={s.music ?? ""}>
            {s.music ? s.music.split("/").pop() : "No music"}
          </span>
          <button type="button" className="btn small" onClick={props.chooseMusic}>{s.music ? "Change" : "Choose…"}</button>
          {s.music && (
            <button type="button" className="icon-btn" onClick={() => update({ music: null })} aria-label="Remove music">✕</button>
          )}
        </div>
        <Slider label="Music volume" value={s.musicVolume} onChange={(musicVolume) => update({ musicVolume })} disabled={!s.music} />
        <Slider
          label="Start the song at"
          value={s.musicStartSec}
          min={0}
          max={180}
          step={1}
          suffix=" s"
          onChange={(musicStartSec) => update({ musicStartSec })}
          disabled={!s.music}
        />
        <Toggle label="Fade music in and out" checked={s.musicFade} onChange={(musicFade) => update({ musicFade })} disabled={!s.music} />
        {travel ? (
          <Toggle label="Cut on the beat" hint="Needs music with a steady beat" checked={s.beatSync} onChange={(beatSync) => update({ beatSync })} disabled={!s.music} />
        ) : (
          <Toggle label="Lower music while talking" checked={s.duckMusic} onChange={(duckMusic) => update({ duckMusic })} disabled={!s.music} />
        )}
        <p className="note">Use music you have the rights to.</p>
      </Section>
      <Section title="Video sound">
        <Toggle label="Mute the video's own sound" checked={s.muteVideo} onChange={(muteVideo) => update({ muteVideo })} />
        <Slider label="Video volume" value={s.videoVolume} onChange={(videoVolume) => update({ videoVolume })} disabled={s.muteVideo} />
        {!travel && <Toggle label="Even out the voice level" checked={s.loudness} onChange={(loudness) => update({ loudness })} />}
        {!travel && s.muteVideo && <p className="note warn">Muted: the speech will not be heard in the reel.</p>}
      </Section>
    </>
  );

  const text = (
    <>
      <Section title="Hook, tags and call to action">
        <div className="field">
          <label htmlFor="hook">Hook — first 3 seconds</label>
          <input id="hook" type="text" placeholder="e.g. NANGANGALAY NA KAMAY MO?" value={props.extras.hook}
            onChange={(e) => props.setExtras({ ...props.extras, hook: e.target.value })} />
        </div>
        <div className="field">
          <label htmlFor="tags">Tags — one per line{travel ? "" : ' (add "| phrase" to show it when that is said)'}</label>
          <textarea id="tags" rows={4} placeholder={travel ? "ISLAND HOPPING\nSUNSET AT CLOUD 9" : "ERGONOMIC MOUSE | ergonomic\nBLUETOOTH + DESKTOP | bluetooth"}
            value={props.extras.tags} onChange={(e) => props.setExtras({ ...props.extras, tags: e.target.value })} />
        </div>
        <div className="field">
          <label htmlFor="cta">Call to action — last seconds</label>
          <input id="cta" type="text" placeholder="e.g. CLICK THE YELLOW BASKET" value={s.cta} onChange={(e) => update({ cta: e.target.value })} />
        </div>
      </Section>
      {!travel && (
        <Section title="Caption check">
          <Toggle label="Check the captions before rendering" hint="Fix spelling, remove lines" checked={s.reviewCaptions} onChange={(reviewCaptions) => update({ reviewCaptions })} />
          <Toggle label="Remember my spelling fixes" hint="Applied to the next videos" checked={s.rememberFixes} onChange={(rememberFixes) => update({ rememberFixes })} disabled={!s.reviewCaptions} />
        </Section>
      )}
    </>
  );

  const output = (
    <>
      <Section title="Shapes">
        <Toggle
          label="Also make the other shape"
          hint={s.orientation === "vertical" ? "Adds a horizontal 16:9 version" : "Adds a vertical 9:16 version"}
          checked={s.bothShapes}
          onChange={(bothShapes) => update({ bothShapes })}
        />
      </Section>
      {!travel && (
        <Section title="Captions">
          <Segmented
            label="Captions"
            value={s.burnCaptions ? "burn" : "srt"}
            options={[
              { value: "burn", label: "In the video" },
              { value: "srt", label: "Separate .srt (CapCut)" },
            ]}
            onChange={(v) => update({ burnCaptions: v === "burn" })}
          />
          <p className="note">
            {s.burnCaptions
              ? "Captions are drawn into the video. A .srt file is saved next to it too."
              : "The video has no captions. Import the .srt into CapCut and style them there."}
          </p>
        </Section>
      )}
      <Section title="Files">
        <p className="note">Reels are saved in Movies › AI Reel Maker, one folder per edit. Nothing is uploaded.</p>
      </Section>
    </>
  );

  return (
    <aside className="inspector" aria-label="Edit settings">
      <div className="inspector-top">
        <Segmented
          label="Kind of video"
          value={s.mode}
          options={[
            { value: "talk", label: "🎙 Talking video" },
            { value: "travel", label: "🌴 Travel b-roll" },
          ]}
          onChange={(mode) => update({ mode })}
        />
        <Tabs
          value={tab}
          tabs={[
            { value: "style", label: "Style" },
            { value: "audio", label: "Audio" },
            { value: "text", label: "Text" },
            { value: "output", label: "Output" },
          ]}
          onChange={setTab}
        />
      </div>
      <div className="inspector-body">
        {tab === "style" && style}
        {tab === "audio" && audio}
        {tab === "text" && text}
        {tab === "output" && output}
      </div>
      <div className="inspector-actions">
        <button type="button" className="btn primary block" onClick={props.onRun} disabled={!!props.blocker || props.running}>
          {travel ? "Make travel reel" : s.reviewCaptions ? "Auto Edit — check captions" : "Auto Edit"}
        </button>
        {travel && (
          <button type="button" className="btn block" onClick={props.onPreview} disabled={!!props.blocker || props.running}>
            ▶ Preview first 10 s
          </button>
        )}
        <p className="note center" title={props.blocker ?? props.target ?? ""}>{props.blocker ?? (props.target ? `Uses ${props.target}` : "")}</p>
      </div>
    </aside>
  );
}
