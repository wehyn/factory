import { Handle, Position, type NodeProps } from "@xyflow/react";

export type AgentNodeData = {
  title: string;
  subtitle: string;
  state: string;
  outputs: string[];
  role: "manager" | "builder";
};

export function AgentNode({ data }: NodeProps) {
  const node = data as AgentNodeData;
  const output = node.outputs.slice(-3);
  return (
    <article className={`agent-node agent-node--${node.state}`} aria-label={`${node.title} session`}>
      <Handle type="target" position={Position.Top} />
      <header className="agent-node__head">
        <span className={`agent-avatar agent-avatar--${node.role}`} aria-hidden="true">
          {node.role === "manager" ? "M" : "B"}
        </span>
        <span className="agent-identity">
          <strong>{node.title}</strong>
          <small>{node.subtitle}</small>
        </span>
        <span className="agent-state">{node.state.replace(/_/g, " ")}</span>
      </header>
      <div className="agent-node__output" aria-label={`${node.title} read-only output`}>
        {output.length ? output.map((line, index) => <pre key={`${index}-${line}`}>{line}</pre>) : (
          <p>Waiting for the first Codex response.</p>
        )}
      </div>
      <footer>Read-only session · output stored locally</footer>
      <Handle type="source" position={Position.Bottom} />
    </article>
  );
}
