import { useCallback, useEffect, useMemo, useState } from "react";
import "@xyflow/react/dist/style.css";
import "./styles.css";
import { FactoryHome } from "./FactoryHome";
import { ManagerChat } from "./ManagerChat";
import { RunCanvas } from "./RunCanvas";
import {
  getFactorySnapshot,
  sendManagerMessage,
  subscribeFactory,
  type AgentMessage,
  type FactoryHomeSnapshot,
} from "./bridge";

function App() {
  const [snapshot, setSnapshot] = useState<FactoryHomeSnapshot | null>(null);
  const [selectedRunId, setSelectedRunId] = useState<string | null>(null);
  const [selectedMessage, setSelectedMessage] = useState<AgentMessage>();
  const [error, setError] = useState("");

  useEffect(() => {
    let cancelled = false;
    let unsubscribe: (() => void) | undefined;
    void subscribeFactory(
      (nextSnapshot) => {
        if (cancelled) return;
        setSnapshot(nextSnapshot);
        setSelectedRunId((current) => current && nextSnapshot.runs.some(({ run }) => run.id === current)
          ? current
          : nextSnapshot.runs[0]?.run.id ?? null);
        setError("");
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
        if (!cancelled) setError(reason instanceof Error ? reason.message : String(reason));
      });
    return () => {
      cancelled = true;
      unsubscribe?.();
    };
  }, []);

  const refresh = useCallback(async () => {
    const next = await getFactorySnapshot();
    setSnapshot(next);
    return next;
  }, []);

  const selectedRun = useMemo(
    () => snapshot?.runs.find(({ run }) => run.id === selectedRunId),
    [snapshot, selectedRunId],
  );

  async function submitManagerMessage(content: string) {
    await sendManagerMessage(content, selectedRunId);
    await refresh();
  }

  function selectRun(runId: string) {
    setSelectedRunId(runId);
    setSelectedMessage(undefined);
  }

  return (
    <main className="app-shell">
      <header className="topbar">
        <div className="brand">
          <div className="brand-mark" aria-hidden="true">A</div>
          <div><div className="brand-name">AGENTIC FACTORY</div><div className="brand-sub">Local agent workspace</div></div>
        </div>
        <div className="top-sep" />
        <div className="project-title">
          <span className="project-kicker">Selected run</span>
          <span className="project-name">{selectedRun?.run.title ?? "No run selected"}</span>
        </div>
        <div className="top-spacer" />
        <span className={`service-pill ${snapshot ? "is-live" : "is-pending"}`} aria-live="polite">
          <i />{snapshot ? "Service connected" : "Connecting to local service"}
        </span>
        {snapshot?.manager_turn_active && <span className="working-pill"><i />Manager working</span>}
      </header>
      {!snapshot ? (
        <section className="startup-state" aria-live="polite">
          <span className="startup-mark">⌁</span>
          <h1>Connecting to Agentic Factory</h1>
          <p>Loading local repositories, runs, and session history.</p>
          {error && <p className="error-banner" role="alert">{error}</p>}
        </section>
      ) : (
        <div className="workspace-layout">
          <ManagerChat snapshot={snapshot} selectedRun={selectedRun} onSend={submitManagerMessage} />
          <RunCanvas
            run={selectedRun}
            snapshot={snapshot}
            onMessageSelect={setSelectedMessage}
          />
          <FactoryHome
            snapshot={snapshot}
            selectedRunId={selectedRunId}
            selectedMessage={selectedMessage}
            error={error}
            onSelectRun={selectRun}
            onClearMessage={() => setSelectedMessage(undefined)}
            onRefresh={refresh}
          />
        </div>
      )}
    </main>
  );
}

export default App;
