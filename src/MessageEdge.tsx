import {
  BaseEdge,
  EdgeLabelRenderer,
  getBezierPath,
  type Edge,
  type EdgeProps,
} from "@xyflow/react";
import type { AgentMessage } from "./bridge";

export type MessageEdgeData = {
  message: AgentMessage;
  accessibleLabel: string;
  onSelect: () => void;
};

export type MessageFlowEdge = Edge<MessageEdgeData, "message">;

export function MessageEdge(props: EdgeProps<MessageFlowEdge>) {
  const [path, labelX, labelY] = getBezierPath({
    sourceX: props.sourceX,
    sourceY: props.sourceY,
    sourcePosition: props.sourcePosition,
    targetX: props.targetX,
    targetY: props.targetY,
    targetPosition: props.targetPosition,
  });
  return (
    <>
      <BaseEdge id={props.id} path={path} markerEnd={props.markerEnd} />
      <EdgeLabelRenderer>
        <button
          type="button"
          className="message-edge-label"
          aria-label={props.data?.accessibleLabel}
          onClick={(event) => {
            event.stopPropagation();
            props.data?.onSelect();
          }}
          style={{ transform: `translate(-50%, -50%) translate(${labelX}px, ${labelY}px)` }}
        >
          {props.data?.message.kind ?? "Message"}
        </button>
      </EdgeLabelRenderer>
    </>
  );
}
