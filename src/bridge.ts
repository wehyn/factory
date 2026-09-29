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
  production_gate: string;
  worktrees: WorktreeHomeView[];
  agents: AgentCanvasView[];
  messages: AgentMessage[];
  blockers: SchedulerBlocker[];
  linked_run_ids: string[];
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
