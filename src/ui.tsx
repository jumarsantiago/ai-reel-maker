// Small, consistent UI building blocks (switches, sliders, segmented controls, tabs, dialogs).
import { ReactNode, useEffect, useId } from "react";

export function Segmented<T extends string>(props: {
  label: string;
  value: T;
  options: { value: T; label: string }[];
  onChange: (v: T) => void;
}) {
  return (
    <div className="segmented" role="radiogroup" aria-label={props.label}>
      {props.options.map((o) => (
        <button
          key={o.value}
          type="button"
          role="radio"
          aria-checked={props.value === o.value}
          className={props.value === o.value ? "on" : ""}
          onClick={() => props.onChange(o.value)}
        >
          {o.label}
        </button>
      ))}
    </div>
  );
}

export function Toggle(props: { label: string; hint?: string; checked: boolean; onChange: (v: boolean) => void; disabled?: boolean }) {
  const id = useId();
  return (
    <label className={`toggle${props.disabled ? " disabled" : ""}`} htmlFor={id}>
      <span className="toggle-text">
        <span>{props.label}</span>
        {props.hint && <span className="hint">{props.hint}</span>}
      </span>
      <input
        id={id}
        type="checkbox"
        role="switch"
        checked={props.checked}
        disabled={props.disabled}
        onChange={(e) => props.onChange(e.target.checked)}
      />
      <span className="switch" aria-hidden="true" />
    </label>
  );
}

export function Slider(props: {
  label: string;
  value: number;
  onChange: (v: number) => void;
  min?: number;
  max?: number;
  step?: number;
  suffix?: string;
  disabled?: boolean;
}) {
  const id = useId();
  const { min = 0, max = 100, step = 1, suffix = "%" } = props;
  return (
    <div className={`slider${props.disabled ? " disabled" : ""}`}>
      <div className="slider-head">
        <label htmlFor={id}>{props.label}</label>
        <output htmlFor={id}>{props.disabled ? "—" : `${props.value}${suffix}`}</output>
      </div>
      <input
        id={id}
        type="range"
        min={min}
        max={max}
        step={step}
        value={props.value}
        disabled={props.disabled}
        style={{ ["--fill" as string]: `${((props.value - min) / (max - min)) * 100}%` }}
        onChange={(e) => props.onChange(Number(e.target.value))}
      />
    </div>
  );
}

export function SelectField<T extends string | number>(props: {
  label: string;
  value: T;
  options: { value: T; label: string }[];
  onChange: (v: T) => void;
}) {
  const id = useId();
  return (
    <div className="field">
      <label htmlFor={id}>{props.label}</label>
      <select
        id={id}
        value={String(props.value)}
        onChange={(e) => {
          const hit = props.options.find((o) => String(o.value) === e.target.value);
          if (hit) props.onChange(hit.value);
        }}
      >
        {props.options.map((o) => (
          <option key={String(o.value)} value={String(o.value)}>{o.label}</option>
        ))}
      </select>
    </div>
  );
}

export function Section(props: { title: string; children: ReactNode; aside?: ReactNode }) {
  return (
    <section className="section">
      <div className="section-head">
        <h3>{props.title}</h3>
        {props.aside}
      </div>
      <div className="section-body">{props.children}</div>
    </section>
  );
}

export function Tabs<T extends string>(props: { value: T; tabs: { value: T; label: string }[]; onChange: (v: T) => void }) {
  return (
    <div className="tabs" role="tablist">
      {props.tabs.map((t) => (
        <button
          key={t.value}
          type="button"
          role="tab"
          aria-selected={props.value === t.value}
          className={props.value === t.value ? "on" : ""}
          onClick={() => props.onChange(t.value)}
        >
          {t.label}
        </button>
      ))}
    </div>
  );
}

/** A centred dialog. `onClose` (when given) also closes on Escape and on a click outside. */
export function Modal(props: { title: string; children: ReactNode; footer?: ReactNode; wide?: boolean; onClose?: () => void }) {
  const id = useId();
  const { onClose } = props;
  useEffect(() => {
    if (!onClose) return;
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);
  return (
    <div className="modal-backdrop" onMouseDown={(e) => onClose && e.target === e.currentTarget && onClose()}>
      <div className={`modal${props.wide ? " wide" : ""}`} role="dialog" aria-modal="true" aria-labelledby={id}>
        <header className="modal-head">
          <h2 id={id}>{props.title}</h2>
          {onClose && (
            <button type="button" className="icon-btn" onClick={onClose} aria-label="Close">✕</button>
          )}
        </header>
        <div className="modal-body">{props.children}</div>
        {props.footer && <footer className="modal-foot">{props.footer}</footer>}
      </div>
    </div>
  );
}
