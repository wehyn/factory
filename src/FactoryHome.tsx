import { useMemo, useState, type FormEvent } from "react";
import {
  createRun as createNamedRun,
  linkRuns,
  registerRepository as registerRepositoryAtPath,
  type AgentMessage,
  type FactoryHomeSnapshot,
  type RunHomeView,
} from "./bridge";

type Props = {
  snapshot: FactoryHomeSnapshot;
  selectedRunId: string | null;
  selectedMessage?: AgentMessage;
  error: string;
  onSelectRun: (runId: string) => void;
  onClearMessage: () => void;
  onRefresh: () => Promise<FactoryHomeSnapshot>;
};

function partyName(run: RunHomeView, party: AgentMessage["from"]) {
  if (party.kind === "manager") return "Manager";
  const index = run.agents.findIndex(({ assignment }) => assignment.agent_id === party.agent_id);
  const assignment = run.agents[index]?.assignment;
  if (assignment?.assignment_key.toLowerCase().includes("api")) return "API builder";
  if (assignment?.assignment_key.toLowerCase().includes("ui")) return "UI builder";
  return `Builder ${index + 1}`;
}

function FactoryHome({ snapshot, selectedRunId, selectedMessage, error, onSelectRun, onClearMessage, onRefresh }: Props) {
  const [repositoryPath, setRepositoryPath] = useState("");
  const [selectedRepoId, setSelectedRepoId] = useState(snapshot.repositories[0]?.id ?? "");
  const [runTitle, setRunTitle] = useState("");
  const [linkTargetId, setLinkTargetId] = useState("");
  const [busy, setBusy] = useState("");
  const [formError, setFormError] = useState("");
  const selectedRun = snapshot.runs.find(({ run }) => run.id === selectedRunId);
  const linkableRuns = useMemo(() => snapshot.runs.filter(({ run }) =>
    run.id !== selectedRunId && !selectedRun?.linked_run_ids.includes(run.id),
  ), [snapshot.runs, selectedRunId, selectedRun]);

  async function registerRepository(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!repositoryPath.trim() || busy) return;
    setBusy("repository");
    setFormError("");
    try {
      await registerRepositoryAtPath(repositoryPath.trim());
      setRepositoryPath("");
      const next = await onRefresh();
      const added = next.repositories[next.repositories.length - 1];
      if (added && !selectedRepoId) setSelectedRepoId(added.id);
    } catch (reason) {
      setFormError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      setBusy("");
    }
  }

  async function createRun(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!selectedRepoId || !runTitle.trim() || busy) return;
    setBusy("run");
    setFormError("");
    try {
      const created = await createNamedRun(selectedRepoId, runTitle.trim());
      setRunTitle("");
      const next = await onRefresh();
      const actual = next.runs.find(({ run }) => run.id === created.id);
      if (actual) onSelectRun(actual.run.id);
    } catch (reason) {
      setFormError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      setBusy("");
    }
  }

  async function linkRun() {
    if (!selectedRunId || !linkTargetId || busy) return;
    setBusy("link");
    setFormError("");
    try {
      await linkRuns(selectedRunId, linkTargetId);
      setLinkTargetId("");
      await onRefresh();
    } catch (reason) {
      setFormError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      setBusy("");
    }
  }

  return (
    <aside className="inspector" aria-label="Factory workspace">
      <div className="panel-heading inspector-heading">
        <span className="heading-label">Workspace</span>
        <span className="repo-count">{snapshot.repositories.length} repos · {snapshot.runs.length} runs</span>
      </div>
      <div className="inspector-scroll">
        <section className="inspector-section" aria-labelledby="repos-title">
          <div className="section-title-row"><h2 id="repos-title">Repositories</h2><span>Local Git</span></div>
          <form className="compact-form" onSubmit={registerRepository}>
            <label htmlFor="repository-path">Repository path</label>
            <div className="input-action-row">
              <input id="repository-path" value={repositoryPath} onChange={(event) => setRepositoryPath(event.target.value)} placeholder="/path/to/repository" />
              <button type="submit" aria-label="Add repository" disabled={!repositoryPath.trim() || busy === "repository"}>{busy === "repository" ? "…" : "+"}</button>
            </div>
          </form>
          <div className="repo-list-compact">
            {snapshot.repositories.map((repository) => (
              <button
                className={`repo-row ${selectedRepoId === repository.id ? "selected" : ""}`}
                key={repository.id}
                onClick={() => setSelectedRepoId(repository.id)}
                type="button"
                aria-label={`Select repository ${repository.canonical_root}`}
              >
                <span className="repo-icon" aria-hidden="true">⌘</span>
                <span className="repo-row__copy"><strong>{repositoryName(repository.canonical_root)}</strong><small>{repository.canonical_root}</small></span>
              </button>
            ))}
            {snapshot.repositories.length === 0 && <p className="muted-note">Add a local Git repository to create a run.</p>}
          </div>
          <form className="compact-form run-create-form" onSubmit={createRun}>
            <label htmlFor="run-repository">Create a run</label>
            <select id="run-repository" value={selectedRepoId} onChange={(event) => setSelectedRepoId(event.target.value)} disabled={!snapshot.repositories.length}>
              {snapshot.repositories.map((repository) => <option value={repository.id} key={repository.id}>{repositoryName(repository.canonical_root)}</option>)}
            </select>
            <input aria-label="New run title" value={runTitle} onChange={(event) => setRunTitle(event.target.value)} placeholder="New run title" maxLength={256} />
            <button className="primary-action" type="submit" disabled={!selectedRepoId || !runTitle.trim() || busy === "run"}>{busy === "run" ? "Creating…" : "Create run"}</button>
          </form>
        </section>

        <section className="inspector-section run-section" aria-labelledby="runs-title">
          <div className="section-title-row"><h2 id="runs-title">Runs</h2><span>{snapshot.runs.length}</span></div>
          <div className="run-list-compact">
            {snapshot.runs.map(({ run, repository, status }) => (
              <button className={`run-row ${selectedRunId === run.id ? "selected" : ""}`} type="button" key={run.id} onClick={() => onSelectRun(run.id)}>
                <span className={`run-status-dot run-status-dot--${status}`} />
                <span className="run-row__copy"><strong>{run.title}</strong><small>{repositoryName(repository.canonical_root)} · {status.replace(/_/g, " ")}</small></span>
                {selectedRunId === run.id && <span className="run-chevron" aria-hidden="true">›</span>}
              </button>
            ))}
            {!snapshot.runs.length && <p className="muted-note">No runs yet. Create one above.</p>}
          </div>
        </section>

        {selectedRun && <>
          <section className="inspector-section" aria-labelledby="gates-title">
            <div className="section-title-row"><h2 id="gates-title">Run gates</h2><span className="context-tag">{selectedRun.status.replace(/_/g, " ")}</span></div>
            <div className="gate-list">
              <Gate label="Integration" value={selectedRun.integration_gate} />
              <Gate label="Pull request" value={selectedRun.pr_gate} />
              <Gate label="Production" value={selectedRun.production_gate} />
            </div>
            {selectedRun.blockers.map((blocker) => <p className="blocker-note" key={blocker.id}>{blocker.detail}</p>)}
          </section>
          <section className="inspector-section" aria-labelledby="worktrees-title">
            <div className="section-title-row"><h2 id="worktrees-title">Worktrees</h2><span>{selectedRun.worktrees.length}</span></div>
            <div className="worktree-list">
              {selectedRun.worktrees.map(({ worktree, status }) => (
                <div className="worktree-row" key={worktree.id}>
                  <strong>{worktree.role.kind === "integration" ? "Integration" : worktree.role.agent_id}</strong>
                  <small>{worktree.branch_name}</small>
                  <span className={`worktree-state worktree-state--${status}`}>{status.replace(/_/g, " ")}</span>
                  <small className="worktree-path">{worktree.path}</small>
                </div>
              ))}
              {!selectedRun.worktrees.length && <p className="muted-note">Integration worktree is preparing.</p>}
            </div>
            {linkableRuns.length > 0 && <div className="link-runs">
              <label htmlFor="link-run">Link a related run</label>
              <div className="input-action-row">
                <select id="link-run" value={linkTargetId} onChange={(event) => setLinkTargetId(event.target.value)}>
                  <option value="">Choose a run</option>
                  {linkableRuns.map(({ run }) => <option value={run.id} key={run.id}>{run.title}</option>)}
                </select>
                <button type="button" onClick={linkRun} disabled={!linkTargetId || busy === "link"} aria-label="Link selected runs">↗</button>
              </div>
            </div>}
          </section>
        </>}

        {selectedMessage && selectedRun && <section className="inspector-section message-detail" aria-labelledby="message-title">
          <div className="section-title-row"><h2 id="message-title">Message provenance</h2><button type="button" className="quiet-button" onClick={onClearMessage} aria-label="Close message provenance">×</button></div>
          <strong className="message-route">{partyName(selectedRun, selectedMessage.from)} → {partyName(selectedRun, selectedMessage.to)}</strong>
          <span className="message-kind">{selectedMessage.kind}</span>
          <p>{selectedMessage.body}</p>
          <small>Run: {selectedRun.run.title} · {new Date(selectedMessage.created_at_ms).toLocaleString()}</small>
        </section>}
        {(formError || error) && <p className="error-banner" role="alert">{formError || error}</p>}
      </div>
    </aside>
  );
}

function Gate({ label, value }: { label: string; value: string }) {
  const state = value.toLowerCase().includes("passed") || value.toLowerCase().includes("healthy")
    ? "passed"
    : value.toLowerCase().includes("blocked") || value.toLowerCase().includes("fail")
      ? "blocked"
      : "waiting";
  return <div className="gate-row"><span className={`gate-dot gate-dot--${state}`} /><span>{label}</span><strong>{value.replace(/_/g, " ")}</strong></div>;
}

function repositoryName(path: string): string {
  const segments = path.split("/").filter(Boolean);
  return segments[segments.length - 1] ?? path;
}

export { FactoryHome };
