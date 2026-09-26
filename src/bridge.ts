import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

export type SessionProcessState = "starting" | "running" | "completed" | "interrupted" | "failed";

export type SessionEventKind =
  | { type: "session_created" }
  | { type: "session_started"; data: { thread_id: string } }
  | { type: "turn_started"; data: { turn_id: string } }
  | { type: "output"; data: string }
  | { type: "turn_completed" }
  | { type: "session_interrupted" }
  | { type: "session_failed"; data: { message: string } };

export type SessionEvent = {
  id: string;
  session_id: string;
  kind: SessionEventKind;
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

export type FactorySnapshot = {
  last_sequence: number;
  sessions: SessionSnapshot[];
};

export async function getSnapshot(): Promise<FactorySnapshot> {
  return invoke<FactorySnapshot>("get_snapshot");
}

export async function getDisposableRepoPath(): Promise<string> {
  return invoke<string>("get_disposable_repo_path");
}

export async function startDisposableSession(repoPath: string, prompt: string): Promise<string> {
  return invoke<string>("start_disposable_session", { repoPath, prompt });
}

export function applyFactoryEvent(
  snapshot: FactorySnapshot,
  sequenced: SequencedFactoryEvent,
): FactorySnapshot {
  const event = sequenced.event;
  const sessions = [...snapshot.sessions];
  let index = sessions.findIndex((session) => session.session_id === event.session_id);

  if (index < 0) {
    index = sessions.length;
    sessions.push({
      session_id: event.session_id,
      process_state: "starting",
      thread_id: null,
      output: [],
      failure_count: 0,
      last_sequence: sequenced.sequence,
      created_at_ms: event.created_at_ms,
    });
  }

  const current = sessions[index];
  let next: SessionSnapshot = { ...current, last_sequence: sequenced.sequence };
  switch (event.kind.type) {
    case "session_created":
      next = { ...next, process_state: "starting", created_at_ms: event.created_at_ms };
      break;
    case "session_started":
      next = { ...next, process_state: "running", thread_id: event.kind.data.thread_id };
      break;
    case "turn_started":
      next = { ...next, process_state: "running" };
      break;
    case "output":
      next = { ...next, output: [...next.output, event.kind.data].slice(-40) };
      break;
    case "turn_completed":
      next = { ...next, process_state: "completed" };
      break;
    case "session_interrupted":
      next = { ...next, process_state: "interrupted" };
      break;
    case "session_failed":
      next = { ...next, process_state: "failed", failure_count: next.failure_count + 1 };
      break;
  }

  sessions[index] = next;
  return { last_sequence: sequenced.sequence, sessions };
}

export async function subscribeFactory(
  onSnapshot: (snapshot: FactorySnapshot) => void,
  onError?: (error: unknown) => void,
): Promise<() => void> {
  let current: FactorySnapshot | undefined;
  let closed = false;
  let queued = Promise.resolve();
  const buffered: SequencedFactoryEvent[] = [];

  const unlisten = await listen<SequencedFactoryEvent>("factory-event", ({ payload }) => {
    queued = queued.then(async () => {
      if (closed) return;
      if (!current) {
        buffered.push(payload);
        return;
      }
      await accept(payload);
    }).catch((error: unknown) => onError?.(error));
  });

  const reload = async () => {
    current = await getSnapshot();
    onSnapshot(current);
  };

  const accept = async (payload: SequencedFactoryEvent) => {
    if (!current || payload.sequence <= current.last_sequence) return;
    if (payload.sequence !== current.last_sequence + 1) {
      await reload();
      return;
    }
    current = applyFactoryEvent(current, payload);
    onSnapshot(current);
  };

  try {
    await reload();
    for (const payload of buffered.splice(0)) await accept(payload);
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
