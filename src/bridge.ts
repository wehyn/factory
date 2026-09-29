import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

export type SessionProcessState = "starting" | "running" | "completed" | "interrupted" | "failed";
export type SliceStatus =
  | "preparing"
  | "waiting_for_contract"
  | "queued"
  | "running"
  | "retryable"
  | "completed"
  | "integrated"
  | "paused"
  | "blocked";
export type WorktreeStatus =
  | "clean"
  | "dirty"
  | "busy"
  | "missing"
  | "unsafe"
  | "creating"
  | "archived"
  | "recovery_required";

export type SessionEvent = {
  id: string;
  session_id: string;
  kind: { type: string; data?: unknown };
  created_at_ms: number;
};

export type SequencedFactoryEvent = {
  sequence: number;
  event: SessionEvent;
};

export type SessionSnapshot = {
  session_id: string;
  process_state: SessionProcessState;
  thread_id: string | null;
  output: string[];
  failure_count: number;
  last_sequence: number;
  created_at_ms: number;
};

export type ManagerChatMessage = {
  id: string;
  session_id: string;
  run_id: string | null;
  role: "user" | "assistant";
  content: string;
  created_at_ms: number;
};

export type Repository = {
  id: string;
  canonical_root: string;
  remote_url: string | null;
  default_branch: string;
  registered_at_ms: number;
};

export type RunRecord = {
  id: string;
  repo_id: string;
  title: string;
  base_sha: string;
  created_at_ms: number;
};

export type Worktree = {
  id: string;
  repo_id: string;
  run_id: string;
  role: { kind: "integration" } | { kind: "agent"; agent_id: string };
  base_sha: string;
  branch_name: string;
  path: string;
  state: "creating" | "active" | "archived" | "recovery_required";
  created_at_ms: number;
};

export type SliceAssignment = {
  id: string;
  run_id: string;
  assignment_key: string;
  objective: string;
  acceptance_evidence: string;
  allowed_paths: string[];
  dependency_ids: string[];
  contract_keys: string[];
  agent_id: string;
  worktree_id: string | null;
  attempt_count: number;
  source_commit: string | null;
  completion_evidence: string | null;
  status: SliceStatus;
  blocked_reason: string | null;
  created_at_ms: number;
};

export type MessageRecipient =
  | { kind: "manager" }
  | { kind: "agent"; agent_id: string };

export type AgentMessage = {
  id: string;
  run_id: string;
  from: MessageRecipient;
  to: MessageRecipient;
  kind: "question" | "answer" | "handoff" | "contract" | "blocker" | "completion";
  body: string;
  contract_key: string | null;
  contract_version: number | null;
  created_at_ms: number;
  acknowledged_at_ms: number | null;
};

export type SchedulerBlocker = {
  id: string;
  run_id: string;
  slice_id: string | null;
  kind: string;
  detail: string;
  created_at_ms: number;
  resolved_at_ms: number | null;
};

export type WorktreeHomeView = { worktree: Worktree; status: WorktreeStatus };
export type AgentCanvasView = {
  assignment: SliceAssignment;
  worktree: WorktreeHomeView | null;
  session: SessionSnapshot | null;
};

export type RunHomeView = {
  run: RunRecord;
  repository: Repository;
  status: string;
  integration_ready: boolean;
  integration_gate: string;
  pr_gate: string;
  pull_request: PullRequestRecord | null;
  production_gate: string;
  production: ProductionRunState | null;
  worktrees: WorktreeHomeView[];
  agents: AgentCanvasView[];
  messages: AgentMessage[];
  blockers: SchedulerBlocker[];
  linked_run_ids: string[];
};

export type PullRequestRecord = {
  run_id: string;
  repo_id: string;
  number: number;
  url: string;
  title: string;
  status: "open" | "closed" | "merged";
  is_draft: boolean;
  head_branch: string;
  base_branch: string;
  head_sha: string;
  base_sha: string;
  author_login: string;
  reviews: { author_login: string; state: string; head_sha: string; submitted_at_ms: number }[];
  checks: { name: string; state: string; head_sha: string }[];
  merged_sha: string | null;
  observed_at_ms: number;
  gate: { kind: "auto_merge" } | { kind: "wait_for_review" | "block"; reason: string } | null;
};

export type PullRequestEvidence = {
  change_summary: string;
  verification: string[];
  independent_review: string[];
  decisions: string[];
  limitations: string[];
};

export type ProductionStatus = "healthy" | "waiting_for_deployment" | "failed" | "unverified";
export type ProductionAlert = {
  id: string;
  run_id: string;
  status: ProductionStatus;
  message: string;
  expected_sha: string | null;
  created_at_ms: number;
  acknowledged_at_ms: number | null;
  resolved_at_ms: number | null;
};
export type ProductionRunState = {
  observation: {
    id: string;
    run_id: string;
    expected_sha: string | null;
    deployed_sha: string | null;
    smoke: "passed" | "failed" | "missing";
    environment_id: string | null;
    status: ProductionStatus;
    detail: string;
    observed_at_ms: number;
  } | null;
  alert: ProductionAlert | null;
};
export type MergeAttempt = {
  decision: { kind: "auto_merge" } | { kind: "wait_for_review" | "block"; reason: string };
  pull_request: PullRequestRecord;
  merged: boolean;
};

export type FactoryHomeSnapshot = {
  last_sequence: number;
  sessions: SessionSnapshot[];
  manager_session_id: string | null;
  manager_turn_active: boolean;
  manager_chat: ManagerChatMessage[];
  repositories: Repository[];
  runs: RunHomeView[];
};

export async function getFactorySnapshot(): Promise<FactoryHomeSnapshot> {
  return invoke<FactoryHomeSnapshot>("get_factory_snapshot");
}

export async function registerRepository(path: string): Promise<Repository> {
  return invoke<Repository>("register_repository", { path });
}

export async function createRun(repoId: string, title: string): Promise<RunRecord> {
  return invoke<RunRecord>("create_run", { repoId, title });
}

export async function linkRuns(runId: string, linkedRunId: string): Promise<void> {
  return invoke<void>("link_runs", { runId, linkedRunId });
}

export async function observePullRequest(runId: string, number: number): Promise<PullRequestRecord> {
  return invoke<PullRequestRecord>("observe_pull_request", { runId, number });
}

export async function createRunPullRequest(runId: string, evidence: PullRequestEvidence): Promise<PullRequestRecord> {
  return invoke<PullRequestRecord>("create_run_pull_request", { runId, evidence });
}

export async function tryMergeRunPullRequest(runId: string, number: number): Promise<MergeAttempt> {
  return invoke<MergeAttempt>("try_merge_run_pull_request", { runId, number });
}

export async function acknowledgeProductionAlert(runId: string, alertId: string): Promise<void> {
  return invoke<void>("acknowledge_production_alert", { runId, alertId });
}

export async function refreshProductionWatch(runId: string): Promise<ProductionRunState> {
  return invoke<ProductionRunState>("refresh_production_watch", { runId });
}

export async function sendManagerMessage(content: string, runId: string | null): Promise<void> {
  return invoke<void>("send_manager_message", { content, runId });
}

export function subscribeFactory(
  onSnapshot: (snapshot: FactoryHomeSnapshot) => void,
  onError?: (error: unknown) => void,
): Promise<() => void> {
  return subscribeFactoryInner(onSnapshot, onError);
}

async function subscribeFactoryInner(
  onSnapshot: (snapshot: FactoryHomeSnapshot) => void,
  onError?: (error: unknown) => void,
): Promise<() => void> {
  let current: FactoryHomeSnapshot | undefined;
  let closed = false;
  let queued = Promise.resolve();
  const buffered: SequencedFactoryEvent[] = [];

  const reload = async () => {
    current = await getFactorySnapshot();
    onSnapshot(current);
  };

  const unlisten = await listen<SequencedFactoryEvent>("factory-event", ({ payload }) => {
    queued = queued
      .then(async () => {
        if (closed) return;
        if (!current) {
          buffered.push(payload);
          return;
        }
        if (payload.sequence <= current.last_sequence) return;
        // The service snapshot is authoritative for canvas nodes, messages, and gate state.
        await reload();
      })
      .catch((error: unknown) => onError?.(error));
  });

  try {
    await reload();
    for (const payload of buffered.splice(0)) {
      if (payload.sequence > (current?.last_sequence ?? 0)) await reload();
    }
  } catch (error) {
    closed = true;
    unlisten();
    throw error;
  }

  return () => {
    closed = true;
    unlisten();
  };
}
