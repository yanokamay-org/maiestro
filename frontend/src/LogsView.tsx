import { useEffect, useMemo, useRef, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { api } from "./api";

const LOG_POLL_MS = 2000;
const LOG_LEVELS = ["ERROR", "WARN", "INFO", "DEBUG", "TRACE"] as const;
type LogLevel = (typeof LOG_LEVELS)[number];

// The level sits right after the (whitespace-free) timestamp, e.g.
// "2026-06-02T22:15:03.1Z  WARN span{…}: …" — anchor there so the word "INFO"
// inside a message body can't mis-colour a line.
const LEVEL_RE = /^\S+\s+(ERROR|WARN|INFO|DEBUG|TRACE)\b/;

/** Pull the level token out of a tracing line so we can colour it. */
function lineLevel(line: string): LogLevel | null {
  const m = LEVEL_RE.exec(line);
  return m ? (m[1] as LogLevel) : null;
}

export function LogsView() {
  const [text, setText] = useState("");
  const [filter, setFilter] = useState("");
  const [error, setError] = useState<string | null>(null);
  // Whether the window is on-screen. It hides (rather than closes) on the close
  // button and is re-shown (with focus) from the tray menu, so gate the poll on
  // this to avoid reading the log file every 2s while hidden.
  const [visible, setVisible] = useState(true);
  const scrollRef = useRef<HTMLDivElement>(null);
  // Only auto-scroll when the user is already pinned to the bottom, so reading
  // back through history isn't yanked away on each 2s refresh.
  const pinnedRef = useRef(true);

  // Hide (keep alive) on close; track visibility so the poll can pause. A re-show
  // from the tray focuses the window, which flips `visible` back on.
  useEffect(() => {
    const win = getCurrentWindow();
    const unClose = win.onCloseRequested((event) => {
      event.preventDefault();
      win.hide();
      setVisible(false);
    });
    const unFocus = win.onFocusChanged(({ payload: focused }) => {
      if (focused) setVisible(true);
    });
    return () => { unClose.then((f) => f()); unFocus.then((f) => f()); };
  }, []);

  // Poll the tail every couple seconds while the window is shown.
  useEffect(() => {
    if (!visible) return;
    let active = true;
    const tick = async () => {
      try {
        const t = await api.logsRead();
        if (active) { setText(t); setError(null); }
      } catch (e) {
        if (active) setError(String(e));
      }
    };
    tick();
    const timer = setInterval(tick, LOG_POLL_MS);
    return () => { active = false; clearInterval(timer); };
  }, [visible]);

  const lines = useMemo(() => {
    const all = text.length ? text.split("\n") : [];
    const f = filter.trim().toLowerCase();
    return f ? all.filter((l) => l.toLowerCase().includes(f)) : all;
  }, [text, filter]);

  // After each render that changed the visible lines, stick to the bottom if the
  // user hasn't scrolled up.
  useEffect(() => {
    const el = scrollRef.current;
    if (el && pinnedRef.current) el.scrollTop = el.scrollHeight;
  }, [lines]);

  function onScroll() {
    const el = scrollRef.current;
    if (!el) return;
    pinnedRef.current = el.scrollHeight - el.scrollTop - el.clientHeight < 24;
  }

  return (
    <main className="panel panel--window logs-panel">
      <header className="panel-header">
        <h1>m<span className="ai">AI</span>estro Code</h1>
        <span className="panel-subtitle">Logs</span>
      </header>

      <div className="logs-toolbar">
        <input
          className="text-input logs-filter"
          type="text"
          aria-label="Filter logs"
          placeholder="Filter… e.g. session=28-add-foo or ERROR"
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
          spellCheck={false}
          autoCapitalize="off"
          autoCorrect="off"
        />
        {filter && (
          <button className="btn-clear" onClick={() => setFilter("")} title="Clear filter" aria-label="Clear filter">✕</button>
        )}
        <button className="btn-add" onClick={() => api.logsReveal()} title="Reveal in Finder">Reveal</button>
      </div>

      <div className="logs-view" ref={scrollRef} onScroll={onScroll}>
        {error ? (
          <div className="logs-empty">Couldn’t read logs: {error}</div>
        ) : lines.length === 0 ? (
          <div className="logs-empty">{text.length ? "No lines match the filter." : "No log entries yet today."}</div>
        ) : (
          lines.map((line, i) => {
            const lvl = lineLevel(line);
            return (
              <div key={i} className={`logs-line${lvl ? ` logs-line--${lvl.toLowerCase()}` : ""}`}>
                {line || " "}
              </div>
            );
          })
        )}
      </div>
    </main>
  );
}
