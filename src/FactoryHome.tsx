import { useMemo, useState, type FormEvent } from "react";
import {
  createRunPullRequest,
  createRun as createNamedRun,
  linkRuns,
  acknowledgeProductionAlert,
  observePullRequest,
  refreshProductionWatch,
  tryMergeRunPullRequest,
  registerRepository as registerRepositoryAtPath,
  type AgentMessage,
  type FactoryHomeSnapshot,
  type PullRequestEvidence,
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
  const [pullRequestNumber, setPullRequestNumber] = useState("");
  const [changeSummary, setChangeSummary] = useState("");
  const [verificationEvidence, setVerificationEvidence] = useState("");
  const [reviewEvidence, setReviewEvidence] = useState("");
  const [decisionEvidence, setDecisionEvidence] = useState("");
  const [limitationEvidence, setLimitationEvidence] = useState("");
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

  async function refreshPullRequest() {
    if (!selectedRunId || !pullRequestNumber || busy) return;
    const number = Number(pullRequestNumber);
    if (!Number.isSafeInteger(number) || number < 1) {
      setFormError("Enter a valid pull request number");
      return;
    }
    setBusy("pr");
    setFormError("");
    try {
      await observePullRequest(selectedRunId, number);
      await onRefresh();
    } catch (reason) {
      setFormError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      setBusy("");
    }
  }

  async function createPullRequest() {
    if (!selectedRunId || busy) return;
    if (!changeSummary.trim() || !verificationEvidence.trim()) {
      setFormError("Add a change summary and verification evidence before publishing the branch");
      return;
    }
    setBusy("create-pr");
    setFormError("");
    try {
      const lines = (value: string) => value.split("\n").map((line) => line.trim()).filter(Boolean);
      const evidence: PullRequestEvidence = {
        change_summary: changeSummary.trim(),
        verification: lines(verificationEvidence),
        independent_review: lines(reviewEvidence),
        decisions: lines(decisionEvidence),
        limitations: lines(limitationEvidence),
      };
      const created = await createRunPullRequest(selectedRunId, evidence);
      setPullRequestNumber(String(created.number));
      await onRefresh();
    } catch (reason) {
      setFormError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      setBusy("");
    }
  }

  async function attemptMerge() {
    const pullRequest = selectedRun?.pull_request;
    if (!selectedRunId || !pullRequest || busy) return;
    setBusy("merge-pr");
    setFormError("");
    try {
      const result = await tryMergeRunPullRequest(selectedRunId, pullRequest.number);
      if (!result.merged && result.decision.kind !== "auto_merge") {
        setFormError(result.decision.reason);
      }
      await onRefresh();
    } catch (reason) {
      setFormError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      setBusy("");
    }
  }

  async function runProductionCheck() {
    if (!selectedRunId || busy || selectedRun?.pull_request?.status !== "merged") return;
    setBusy("production");
    setFormError("");
    try {
      await refreshProductionWatch(selectedRunId);
      await onRefresh();
    } catch (reason) {
      setFormError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      setBusy("");
    }
  }

  async function acknowledgeAlert(alertId: string) {
    if (!selectedRunId || busy) return;
    setBusy("acknowledge");
    setFormError("");
    try {
      await acknowledgeProductionAlert(selectedRunId, alertId);
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
            <div className="pr-refresh">
              <label htmlFor="pull-request-number">Track a pull request</label>
              <div className="input-action-row">
                <input id="pull-request-number" type="number" min="1" step="1" value={pullRequestNumber} onChange={(event) => setPullRequestNumber(event.target.value)} placeholder="PR number" />
                <button type="button" onClick={refreshPullRequest} disabled={!pullRequestNumber || busy === "pr"} aria-label="Refresh pull request">{busy === "pr" ? "…" : "↻"}</button>
              </div>
              {selectedRun.pull_request && <a className="pr-link" href={selectedRun.pull_request.url} target="_blank" rel="noreferrer">
                PR #{selectedRun.pull_request.number} · {selectedRun.pull_request.status}
              </a>}
            </div>
            {!selectedRun.pull_request && selectedRun.integration_ready && <details className="pr-evidence-form" open>
              <summary>Prepare pull request</summary>
              <label htmlFor="pr-change-summary">Change summary</label>
              <textarea id="pr-change-summary" value={changeSummary} onChange={(event) => setChangeSummary(event.target.value)} rows={2} maxLength={4000} placeholder="What changed and why?" />
              <label htmlFor="pr-verification">Verification evidence</label>
              <textarea id="pr-verification" value={verificationEvidence} onChange={(event) => setVerificationEvidence(event.target.value)} rows={3} maxLength={4000} placeholder="One command or result per line" />
              <label htmlFor="pr-review">Independent review</label>
              <textarea id="pr-review" value={reviewEvidence} onChange={(event) => setReviewEvidence(event.target.value)} rows={2} maxLength={2000} placeholder="Optional; GitHub current-head approval is still required by policy" />
              <label htmlFor="pr-decisions">Decisions</label>
              <textarea id="pr-decisions" value={decisionEvidence} onChange={(event) => setDecisionEvidence(event.target.value)} rows={2} maxLength={2000} placeholder="One decision per line, if any" />
              <label htmlFor="pr-limitations">Limitations</label>
              <textarea id="pr-limitations" value={limitationEvidence} onChange={(event) => setLimitationEvidence(event.target.value)} rows={2} maxLength={2000} placeholder="One limitation per line, if any" />
              <button className="primary-action" type="button" onClick={createPullRequest} disabled={!changeSummary.trim() || !verificationEvidence.trim() || busy === "create-pr"}>
                {busy === "create-pr" ? "Publishing…" : "Publish branch and create PR"}
              </button>
            </details>}
            {selectedRun.pull_request?.status === "open" && selectedRun.pull_request.gate?.kind === "auto_merge" && <button className="primary-action merge-action" type="button" onClick={attemptMerge} disabled={busy === "merge-pr"}>
              {busy === "merge-pr" ? "Rechecking…" : "Merge with current checks"}
            </button>}
            {selectedRun.production && <div className="production-watch">
              <p className="production-detail">{selectedRun.production.observation?.detail}</p>
              {selectedRun.production.alert && <div className="production-alert" role="status">
                <strong>{selectedRun.production.alert.status.replace(/_/g, " ")}</strong>
                <p>{selectedRun.production.alert.message}</p>
                {selectedRun.production.alert.resolved_at_ms && <small>Resolved</small>}
                {!selectedRun.production.alert.acknowledged_at_ms && <button type="button" onClick={() => acknowledgeAlert(selectedRun.production!.alert!.id)} disabled={busy === "acknowledge"}>
                  {busy === "acknowledge" ? "Saving…" : "Acknowledge"}
                </button>}
              </div>}
            </div>}
            {selectedRun.pull_request?.status === "merged" && <button className="quiet-button production-check" type="button" onClick={runProductionCheck} disabled={busy === "production"}>
              {busy === "production" ? "Checking…" : "Run production check"}
            </button>}
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
  const state = value.toLowerCase().includes("passed") || value.toLowerCase().includes("healthy") || value.toLowerCase().includes("ready to merge") || value.toLowerCase().includes("merged")
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
