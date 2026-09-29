import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import App from "./App";
import type { FactoryHomeSnapshot, SequencedFactoryEvent } from "./bridge";

const tauriMocks = vi.hoisted(() => ({
  invoke: vi.fn(),
  listen: vi.fn(),
  handlers: [] as Array<(event: { payload: unknown }) => void>,
}));

vi.mock("@tauri-apps/api/core", () => ({ invoke: tauriMocks.invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen: tauriMocks.listen }));
vi.mock("@xyflow/react", async () => {
  return {
    Background: () => null,
    Controls: () => null,
    Handle: () => null,
    Position: { Left: "left", Right: "right" },
    MarkerType: { ArrowClosed: "arrowclosed" },
    ReactFlow: ({ nodes, edges, nodeTypes, onEdgeClick }: any) => (
      <div aria-label="Agent canvas" role="group">
        {nodes.map((node: any) => {
          const Node = nodeTypes[node.type];
          return <Node data={node.data} id={node.id} key={node.id} />;
        })}
        {edges.map((edge: any) => (
          <button
            key={edge.id}
            onClick={(event) => onEdgeClick?.(event, edge)}
            type="button"
          >
            {edge.data?.accessibleLabel ?? edge.label ?? "Assignment edge"}
          </button>
        ))}
      </div>
    ),
  };
});

function baseSnapshot(): FactoryHomeSnapshot {
  const repository = {
    id: "repo-1",
    canonical_root: "/Users/wayne/dev/sample-api",
    remote_url: "https://example.invalid/sample-api",
    default_branch: "main",
    registered_at_ms: 1,
  };
  const linkedRepository = {
    ...repository,
    id: "repo-2",
    canonical_root: "/Users/wayne/dev/sample-ui",
    remote_url: null,
  };
  const run = {
    id: "run-1",
    repo_id: repository.id,
    title: "Build API",
    base_sha: "a".repeat(40),
    created_at_ms: 2,
  };
  const linkedRun = {
    id: "run-2",
    repo_id: linkedRepository.id,
    title: "Build UI",
    base_sha: "b".repeat(40),
    created_at_ms: 3,
  };
  const worktree = {
    id: "worktree-1",
    repo_id: repository.id,
    run_id: run.id,
    role: { kind: "agent", agent_id: "agent-1" } as const,
    base_sha: run.base_sha,
    branch_name: "factory/run-1/agent-1",
    path: "/tmp/worktrees/run-1/agent-1",
    state: "active" as const,
    created_at_ms: 4,
  };
  const assignment = {
    id: "slice-1",
    run_id: run.id,
    assignment_key: "api",
    objective: "Implement the API endpoint",
    acceptance_evidence: "API checks pass",
    allowed_paths: ["src/api.rs"],
    dependency_ids: [],
    contract_keys: [],
    agent_id: "agent-1",
    worktree_id: worktree.id,
    attempt_count: 1,
    source_commit: null,
    completion_evidence: null,
    status: "running" as const,
    blocked_reason: null,
    created_at_ms: 5,
  };
  const message = {
    id: "message-1",
    run_id: run.id,
    from: { kind: "agent" as const, agent_id: "agent-1" },
    to: { kind: "manager" as const },
    kind: "completion" as const,
    body: "The API file is ready for review.",
    contract_key: null,
    contract_version: null,
    created_at_ms: 6,
    acknowledged_at_ms: null,
  };

  return {
    last_sequence: 10,
    manager_session_id: "manager-session",
    manager_turn_active: false,
    manager_chat: [
      {
        id: "chat-1",
        session_id: "manager-session",
        run_id: run.id,
        role: "user",
        content: "Build the API and UI in parallel.",
        created_at_ms: 7,
      },
      {
        id: "chat-2",
        session_id: "manager-session",
        run_id: run.id,
        role: "assistant",
        content: "I have split the work into two isolated tasks.",
        created_at_ms: 8,
      },
    ],
    sessions: [
      {
        session_id: "manager-session",
        process_state: "completed",
        thread_id: "thread-manager",
        output: ["I have split the work into two isolated tasks."],
        failure_count: 0,
        last_sequence: 9,
        created_at_ms: 1,
      },
      {
        session_id: "agent-session",
        process_state: "running",
        thread_id: "thread-agent",
        output: ["Reading the API module.", "Checking the acceptance contract."],
        failure_count: 0,
        last_sequence: 10,
        created_at_ms: 4,
      },
    ],
    repositories: [repository, linkedRepository],
    runs: [
      {
        run,
        repository,
        status: "running",
        integration_ready: false,
        integration_gate: "pending",
        pr_gate: "awaiting PR tracking",
        pull_request: null,
        production_gate: "awaiting production watch",
        production: null,
        worktrees: [{ worktree, status: "clean" as const }],
        agents: [{ assignment, worktree: { worktree, status: "clean" as const }, session: {
          session_id: "agent-session",
          process_state: "running",
          thread_id: "thread-agent",
          output: ["Reading the API module.", "Checking the acceptance contract."],
          failure_count: 0,
          last_sequence: 10,
          created_at_ms: 4,
        } }],
        messages: [message],
        blockers: [],
        linked_run_ids: [linkedRun.id],
      },
      {
        run: linkedRun,
        repository: linkedRepository,
        status: "intake",
        integration_ready: false,
        integration_gate: "not_started",
        pr_gate: "awaiting PR tracking",
        pull_request: null,
        production_gate: "awaiting production watch",
        production: null,
        worktrees: [],
        agents: [],
        messages: [],
        blockers: [],
        linked_run_ids: [run.id],
      },
    ],
  };
}

function outputEvent(sequence: number): SequencedFactoryEvent {
  return {
    sequence,
    event: {
      id: `event-${sequence}`,
      session_id: "manager-session",
      kind: { type: "turn_completed" },
      created_at_ms: sequence,
    },
  };
}

describe("Agentic Factory workspace", () => {
  afterEach(cleanup);

  beforeEach(() => {
    vi.clearAllMocks();
    tauriMocks.handlers.length = 0;
    tauriMocks.listen.mockImplementation(
      async (_eventName: string, handler: (event: { payload: unknown }) => void) => {
        tauriMocks.handlers.push(handler);
        return vi.fn();
      },
    );
    tauriMocks.invoke.mockImplementation((command: string) => {
      if (command === "get_factory_snapshot") return Promise.resolve(baseSnapshot());
      if (command === "create_run") return Promise.resolve({ id: "run-created" });
      return Promise.resolve(undefined);
    });
  });

  it("keeps one manager chat across runs and opens a directed message provenance record", async () => {
    render(<App />);
    expect(await screen.findByRole("button", { name: /Build API/i })).toBeVisible();
    expect(screen.getByText("Reading the API module.")).toBeVisible();
    expect(screen.getByText("I have split the work into two isolated tasks.")).toBeVisible();

    fireEvent.click(screen.getByRole("button", { name: /Open message from API builder to Manager/i }));
    expect(await screen.findByText("API builder → Manager")).toBeVisible();
    expect(screen.getByText("The API file is ready for review.")).toBeVisible();
    expect(screen.getByText(/Run: Build API/)).toBeVisible();

    fireEvent.click(screen.getByRole("button", { name: /Build UI/i }));
    expect(screen.getByText("I have split the work into two isolated tasks.")).toBeVisible();
    expect(screen.getByText(/Active context: Build UI/)).toBeVisible();
  });

  it("reloads the service snapshot after a sequence gap and draws newly spawned agents", async () => {
    const initial = baseSnapshot();
    initial.last_sequence = 10;
    initial.runs[0].agents = [];
    initial.sessions = initial.sessions.slice(0, 1);
    const recovered = baseSnapshot();
    recovered.last_sequence = 12;
    tauriMocks.invoke.mockImplementation((command: string) => {
      if (command === "get_factory_snapshot") {
        return Promise.resolve(tauriMocks.invoke.mock.calls.filter(([name]) => name === command).length === 1
          ? initial
          : recovered);
      }
      return Promise.resolve(undefined);
    });

    render(<App />);
    expect(await screen.findByText("No agent sessions yet")).toBeVisible();
    await act(async () => tauriMocks.handlers[0]({ payload: outputEvent(12) }));

    expect(await screen.findByText("Reading the API module.")).toBeVisible();
    await waitFor(() => {
      expect(tauriMocks.invoke.mock.calls.filter(([name]) => name === "get_factory_snapshot")).toHaveLength(2);
    });
  });

  it("registers a repository and creates a named run from the selected repository", async () => {
    render(<App />);
    await screen.findByRole("button", { name: /Build API/i });
    fireEvent.change(screen.getByLabelText("Repository path"), {
      target: { value: "/Users/wayne/dev/new-project" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Add repository" }));
    await waitFor(() => {
      expect(tauriMocks.invoke).toHaveBeenCalledWith("register_repository", {
        path: "/Users/wayne/dev/new-project",
      });
    });

    fireEvent.change(screen.getByLabelText("New run title"), {
      target: { value: "Add search filters" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Create run" }));
    await waitFor(() => {
      expect(tauriMocks.invoke).toHaveBeenCalledWith("create_run", {
        repoId: "repo-1",
        title: "Add search filters",
      });
    });
  });

  it("records PR summary and verification evidence before publishing the integration branch", async () => {
    const ready = baseSnapshot();
    ready.runs[0].integration_ready = true;
    tauriMocks.invoke.mockImplementation((command: string) => {
      if (command === "get_factory_snapshot") return Promise.resolve(ready);
      if (command === "create_run_pull_request") return Promise.resolve({ number: 12 });
      return Promise.resolve(undefined);
    });

    render(<App />);
    await screen.findByRole("button", { name: /Build API/i });
    fireEvent.change(screen.getByLabelText("Change summary"), {
      target: { value: "Add reliable search filters" },
    });
    fireEvent.change(screen.getByLabelText("Verification evidence"), {
      target: { value: "cargo test -p factory-core\nnpm run build" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Publish branch and create PR" }));

    await waitFor(() => {
      expect(tauriMocks.invoke).toHaveBeenCalledWith("create_run_pull_request", {
        runId: "run-1",
        evidence: {
          change_summary: "Add reliable search filters",
          verification: ["cargo test -p factory-core", "npm run build"],
          independent_review: [],
          decisions: [],
          limitations: [],
        },
      });
    });
  });
});
