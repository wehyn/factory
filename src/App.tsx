import { useEffect, useState, type FormEvent } from "react";
import "./styles.css";
import {
  getDisposableRepoPath,
  subscribeFactory,
  startDisposableSession,
  type FactorySnapshot,
  type SessionSnapshot,
} from "./bridge";

const DEFAULT_PROMPT =
  "Reply with exactly the word hello. Do not use tools or modify files.";

function stateLabel(session: SessionSnapshot | undefined) {
  if (!session) return "Ready";
  switch (session.process_state) {
    case "starting": return "Starting";
    case "running": return "Running";
    case "completed": return "Completed";
    case "interrupted": return "Interrupted";
    case "failed": return "Needs attention";
  }
}

function App() {
  const [snapshot, setSnapshot] = useState<FactorySnapshot | null>(null);
  const [repoPath, setRepoPath] = useState("");
  const [prompt, setPrompt] = useState(DEFAULT_PROMPT);
  const [error, setError] = useState("");
  const [starting, setStarting] = useState(false);

  useEffect(() => {
    let cancelled = false;
    let unsubscribe: (() => void) | undefined;

    void subscribeFactory(
      (nextSnapshot) => {
        if (!cancelled) setSnapshot(nextSnapshot);
      },
      (reason) => {
        if (!cancelled) setError(reason instanceof Error ? reason.message : String(reason));
      },
    )
      .then((stop) => {
        if (cancelled) stop();
        else unsubscribe = stop;
      })
      .catch((reason: unknown) => {
        if (!cancelled) {
          setError(reason instanceof Error ? reason.message : String(reason));
        }
      });

    void getDisposableRepoPath()
      .then((path) => {
        if (!cancelled) setRepoPath(path);
      })
      .catch((reason: unknown) => {
        if (!cancelled) {
          setError(reason instanceof Error ? reason.message : String(reason));
        }
      });

    return () => {
      cancelled = true;
      unsubscribe?.();
    };
  }, []);

  const sessions = snapshot?.sessions ?? [];
  const session = sessions.length ? sessions[sessions.length - 1] : undefined;
  const label = stateLabel(session);

  async function startSession(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!repoPath || !prompt.trim() || starting || session) return;
    setStarting(true);
    setError("");
    try {
      await startDisposableSession(repoPath, prompt.trim());
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      setStarting(false);
    }
  }

  return (
    <main className="shell">
      <header className="topbar">
        <div className="brand-mark" aria-hidden="true">A</div>
        <div className="brand-copy">
          <p className="eyebrow">Resident Codex session</p>
          <h1>Agentic Factory</h1>
        </div>
        <div className="service-state" aria-live="polite">
          <span className={snapshot ? "service-dot" : "service-dot service-dot--pending"} />
          {snapshot ? "Service connected" : "Connecting to local service"}
        </div>
      </header>

      <section className="workspace" aria-labelledby="workspace-title">
        <div className="workspace-intro">
          <div>
            <p className="eyebrow">Your local workspace</p>
            <h2 id="workspace-title">A persistent home for Codex work.</h2>
            <p className="welcome-copy">
              Start one real Codex session in a dedicated disposable repository. The app
              keeps the service resident when this window closes and restores output from
              its local event ledger.
            </p>
          </div>
          <div className="status-card">
            <span className="status-card__label">Session status</span>
            <strong>{label}</strong>
            <span className="safety-pill"><span aria-hidden="true" />Read only</span>
          </div>
        </div>

        <div className="content-grid">
          <section className="session-card" aria-labelledby="session-title">
            <div className="section-heading">
              <div>
                <p className="eyebrow">Live session</p>
                <h3 id="session-title">Session output</h3>
              </div>
              {session?.thread_id && <span className="thread-chip">Thread connected</span>}
            </div>

            {session ? (
              <div className="output-list" aria-live="polite" aria-label="Codex session output">
                {session.output.length > 0 ? (
                  session.output.slice(-40).map((output, index) => (
                    <pre className="output-entry" key={`${session.session_id}-${index}`}>
                      {output}
                    </pre>
                  ))
                ) : (
                  <p className="empty-output">Waiting for Codex output…</p>
                )}
                {session.process_state === "failed" && (
                  <p className="session-warning" role="status">
                    The App Server stopped before the turn completed.
                  </p>
                )}
              </div>
            ) : (
              <div className="empty-session">
                <div className="empty-glyph" aria-hidden="true">⌁</div>
                <p>Your first session will appear here.</p>
                <span>Output is real Codex text, redacted and stored on this Mac.</span>
              </div>
            )}
          </section>

          <aside className="launch-card" aria-labelledby="launch-title">
            <div className="section-heading">
              <div>
                <p className="eyebrow">Disposable repository</p>
                <h3 id="launch-title">Start a session</h3>
              </div>
              <span className="lock-icon" aria-hidden="true">⌑</span>
            </div>
            <p className="launch-copy">
              Codex can read this isolated fixture. The repository is outside your projects,
              and the sandbox blocks file changes.
            </p>
            <form onSubmit={startSession}>
              <label htmlFor="session-prompt">Prompt for the disposable repository</label>
              <textarea
                id="session-prompt"
                value={prompt}
                onChange={(event) => setPrompt(event.target.value)}
                maxLength={16_000}
                rows={4}
                disabled={Boolean(session) || starting}
              />
              <button type="submit" disabled={!snapshot || !repoPath || Boolean(session) || starting || !prompt.trim()}>
                {starting ? "Starting…" : session ? "Session already started" : "Start read-only session"}
              </button>
            </form>
            {error && <p className="error-banner" role="alert">{error}</p>}
            <div className="safety-note">
              <span aria-hidden="true">✓</span>
              <p>Read-only sandbox · No approval prompts · Local event history</p>
            </div>
          </aside>
        </div>
      </section>

      <footer className="footer">
        <span>Local first · Read only</span>
        <span>Codex App Server</span>
      </footer>
    </main>
  );
}

export default App;
