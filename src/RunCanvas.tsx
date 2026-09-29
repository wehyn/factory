import { useMemo } from "react";
import type { MouseEvent as ReactMouseEvent } from "react";
import {
  Background,
  Controls,
  MarkerType,
  ReactFlow,
  type Edge,
  type Node,
} from "@xyflow/react";
import type { AgentMessage, FactoryHomeSnapshot, RunHomeView } from "./bridge";
import { AgentNode, type AgentNodeData } from "./AgentNode";
import { MessageEdge, type MessageFlowEdge } from "./MessageEdge";

const nodeTypes = { agent: AgentNode };
const edgeTypes = { message: MessageEdge };

type Props = {
  run?: RunHomeView;
  snapshot: FactoryHomeSnapshot;
  onMessageSelect: (message: AgentMessage) => void;
};

function stateForAgent(runState: string, processState?: string) {
  if (runState === "blocked" || runState === "paused") return "needs_attention";
  if (processState === "failed" || processState === "interrupted") return "needs_attention";
  return processState ?? runState;
}

export function RunCanvas({ run, snapshot, onMessageSelect }: Props) {
  const { nodes, edges } = useMemo(() => {
    if (!run) return { nodes: [] as Node<AgentNodeData>[], edges: [] as Edge[] };

    const managerNode: Node<AgentNodeData> = {
      id: "manager",
      type: "agent",
      position: { x: 300, y: 42 },
      data: {
        title: "Factory manager",
        subtitle: "Persistent conversation",
        state: snapshot.manager_turn_active ? "running" : "ready",
        // Manager prose belongs in the persistent chat panel. Keeping it out of the node
        // avoids rendering a second copy of the global conversation as terminal output.
        outputs: [],
        role: "manager",
      },
    };
    const agentNodes = run.agents.map(({ assignment, session }, index): Node<AgentNodeData> => ({
      id: `agent:${assignment.agent_id}`,
      type: "agent",
      position: { x: 62 + (index % 3) * 322, y: 300 + Math.floor(index / 3) * 220 },
      data: {
        title: `Builder ${index + 1}`,
        subtitle: assignment.objective,
        state: stateForAgent(assignment.status, session?.process_state),
        outputs: session?.output ?? [],
        role: "builder",
      },
    }));
    const assignmentEdges: Edge[] = run.agents.map(({ assignment }) => ({
      id: `assignment:${assignment.id}`,
      source: "manager",
      target: `agent:${assignment.agent_id}`,
      label: assignment.assignment_key,
      type: "default",
      animated: assignment.status === "running",
      style: { stroke: "#4d627b", strokeWidth: 1.5 },
    }));
    const agentsById = new Map(run.agents.map(({ assignment }) => [assignment.agent_id, assignment]));
    const messageEdges: MessageFlowEdge[] = run.messages.flatMap((message) => {
      const source = message.from.kind === "manager" ? "manager" : `agent:${message.from.agent_id}`;
      const target = message.to.kind === "manager" ? "manager" : `agent:${message.to.agent_id}`;
      if (source === target) return [];
      if (message.from.kind === "agent" && !agentsById.has(message.from.agent_id)) return [];
      if (message.to.kind === "agent" && !agentsById.has(message.to.agent_id)) return [];
      const fromName = message.from.kind === "manager"
        ? "Manager"
        : agentName(run, message.from.agent_id);
      const toName = message.to.kind === "manager"
        ? "Manager"
        : agentName(run, message.to.agent_id);
      return [{
        id: `message:${message.id}`,
        source,
        target,
        type: "message",
        label: message.kind,
        markerEnd: { type: MarkerType.ArrowClosed, color: "#b7a2ff" },
        style: { stroke: "#9d87e8", strokeDasharray: "5 5", strokeWidth: 1.6 },
        data: {
          message,
          accessibleLabel: `Open message from ${fromName} to ${toName}`,
          onSelect: () => onMessageSelect(message),
        },
      }];
    });
    return {
      nodes: [managerNode, ...agentNodes],
      edges: [...assignmentEdges, ...messageEdges],
    };
  }, [run, snapshot, onMessageSelect]);

  function selectEdge(_event: ReactMouseEvent, edge: Edge) {
    const data = edge.data as MessageFlowEdge["data"] | undefined;
    if (data?.message) onMessageSelect(data.message);
  }

  return (
    <section className="canvas-panel" aria-label="Run agent canvas">
      <div className="canvas-toolbar">
        <div className="canvas-title">
          <span className="canvas-title__eyebrow">Live run</span>
          <strong>{run?.run.title ?? "Select or create a run"}</strong>
        </div>
        <div className="canvas-legend" aria-label="Canvas legend">
          <span><i className="legend-line legend-line--assignment" /> assignment</span>
          <span><i className="legend-line legend-line--message" /> directed message</span>
          <span>Read-only</span>
        </div>
      </div>
      <div className="canvas-viewport">
        {run ? (
          <ReactFlow
            nodes={nodes}
            edges={edges}
            nodeTypes={nodeTypes}
            edgeTypes={edgeTypes}
            onEdgeClick={selectEdge}
            nodesDraggable={false}
            nodesConnectable={false}
            elementsSelectable={false}
            fitView
            fitViewOptions={{ padding: 0.25 }}
            minZoom={0.35}
            maxZoom={1.5}
            proOptions={{ hideAttribution: true }}
          >
            <Background color="#344257" gap={22} size={1} />
            <Controls showInteractive={false} />
          </ReactFlow>
        ) : (
          <div className="canvas-empty">
            <span className="empty-mark" aria-hidden="true">⌁</span>
            <strong>Your run canvas will appear here.</strong>
            <p>Add a repository, then create a titled run to start assigning work to isolated builders.</p>
          </div>
        )}
      </div>
      {run && run.agents.length === 0 && (
        <div className="canvas-empty-hint" role="status">No agent sessions yet</div>
      )}
    </section>
  );
}

function agentName(run: RunHomeView, agentId: string): string {
  const assignment = run.agents.find(({ assignment }) => assignment.agent_id === agentId)?.assignment;
  const key = assignment?.assignment_key.toLowerCase() ?? "";
  if (key.includes("api")) return "API builder";
  if (key.includes("ui")) return "UI builder";
  return `Builder ${run.agents.findIndex(({ assignment }) => assignment.agent_id === agentId) + 1}`;
}
