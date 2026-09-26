import { act, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import App from "./App";
import {
  applyFactoryEvent,
  type FactorySnapshot,
  type SequencedFactoryEvent,
} from "./bridge";

const tauriMocks = vi.hoisted(() => ({
  invoke: vi.fn(),
  listen: vi.fn(),
  handlers: [] as Array<(event: { payload: unknown }) => void>,
}));

vi.mock("@tauri-apps/api/core", () => ({ invoke: tauriMocks.invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen: tauriMocks.listen }));

function snapshot(lastSequence: number, output: string[]): FactorySnapshot {
  return {
    last_sequence: lastSequence,
    sessions: [
      {
        session_id: "session-1",
        process_state: "running",
        thread_id: "thread-1",
        output,
        failure_count: 0,
        last_sequence: lastSequence,
        created_at_ms: 1,
      },
    ],
  };
}

function outputEvent(sequence: number, text: string): SequencedFactoryEvent {
  return {
    sequence,
    event: {
      id: `event-${sequence}`,
      session_id: "session-1",
      kind: { type: "output", data: text },
      created_at_ms: sequence,
    },
  };
}

describe("resident session view", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    tauriMocks.handlers.length = 0;
  });

  it("shows real session output and reloads after an event gap", async () => {
    const snapshots = [snapshot(1, []), snapshot(4, ["hello", "later"])];
    tauriMocks.invoke.mockImplementation((command: string) => {
      if (command === "get_snapshot") return Promise.resolve(snapshots.shift());
      if (command === "get_disposable_repo_path") return Promise.resolve("/tmp/disposable-repo");
      return Promise.resolve(undefined);
    });
    tauriMocks.listen.mockImplementation(
      async (_eventName: string, handler: (event: { payload: unknown }) => void) => {
        tauriMocks.handlers.push(handler);
        return vi.fn();
      },
    );

    render(<App />);
    await screen.findByText("Read only");

    await act(async () => tauriMocks.handlers[0]({ payload: outputEvent(2, "hello") }));
    expect(await screen.findByText("hello")).toBeVisible();
    expect(screen.queryByRole("textbox", { name: /builder/i })).toBeNull();

    await act(async () => tauriMocks.handlers[0]({ payload: outputEvent(4, "later") }));
    await waitFor(() => {
      expect(
        tauriMocks.invoke.mock.calls.filter(([command]) => command === "get_snapshot"),
      ).toHaveLength(2);
    });
    expect(await screen.findByText("later")).toBeVisible();
  });

  it("keeps live output bounded to the same forty entries as recovered snapshots", () => {
    let current = snapshot(1, []);
    for (let index = 1; index <= 45; index += 1) {
      current = applyFactoryEvent(current, outputEvent(index + 1, `output-${index}`));
    }

    expect(current.sessions[0].output).toHaveLength(40);
    expect(current.sessions[0].output[0]).toBe("output-6");
    expect(current.sessions[0].output[current.sessions[0].output.length - 1]).toBe("output-45");
  });
});
