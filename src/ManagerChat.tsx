import { useState, type FormEvent } from "react";
import type { FactoryHomeSnapshot, RunHomeView } from "./bridge";

type Props = {
  snapshot: FactoryHomeSnapshot;
  selectedRun?: RunHomeView;
  onSend: (content: string) => Promise<void>;
};

export function ManagerChat({ snapshot, selectedRun, onSend }: Props) {
  const [draft, setDraft] = useState("");
  const [sending, setSending] = useState(false);
  const [error, setError] = useState("");

  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const content = draft.trim();
    if (!content || sending || snapshot.manager_turn_active) return;
    setSending(true);
    setError("");
    try {
      await onSend(content);
      setDraft("");
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      setSending(false);
    }
  }

  return (
    <aside className="chat-panel" aria-label="Persistent manager chat">
      <div className="panel-heading">
        <div className="manager-identity">
          <span className="avatar" aria-hidden="true">M</span>
          <div><div className="manager-name">Factory manager</div><div className="manager-role">Your single point of contact</div></div>
        </div>
        <span className="chat-live" aria-label="Local persistent chat">Local</span>
      </div>
      <div className="chat-context">
        <div className="context-top">
          <strong className="context-title">Active context: {selectedRun?.run.title ?? "No run selected"}</strong>
          <span className="context-tag">{selectedRun ? selectedRun.status.replace(/_/g, " ") : "Ready"}</span>
        </div>
        <div className="context-copy">Changing runs keeps this conversation intact. Each message records the run context it was sent with.</div>
      </div>
      <div className="chat-feed" aria-label="Manager conversation" aria-live="polite">
        {snapshot.manager_chat.length ? snapshot.manager_chat.map((message) => (
          <article className={`message ${message.role}`} key={message.id}>
            <div className="message-meta"><strong>{message.role === "user" ? "You" : "Manager"}</strong></div>
            <div className="message-bubble">{message.content}</div>
          </article>
        )) : (
          <div className="chat-welcome">
            <span aria-hidden="true">✳</span>
            <strong>Tell your manager what to build.</strong>
            <p>It can split a run into bounded assignments and coordinate builders in isolated worktrees.</p>
          </div>
        )}
      </div>
      <form className="chat-composer" onSubmit={submit}>
        {snapshot.manager_turn_active && <div className="manager-working" role="status"><span /> Manager is working on this turn</div>}
        <div className="composer-box">
          <textarea
            aria-label="Message the manager"
            placeholder="Describe the outcome you want…"
            value={draft}
            onChange={(event) => setDraft(event.target.value)}
            maxLength={8_000}
            rows={3}
            disabled={sending || snapshot.manager_turn_active}
          />
          <div className="composer-bottom">
            <span>Context: {selectedRun?.run.title ?? "none"}</span>
            <button className="send-button" type="submit" aria-label="Send message to manager" disabled={!draft.trim() || sending || snapshot.manager_turn_active}>
              {sending ? "…" : "↑"}
            </button>
          </div>
        </div>
        {error && <p className="chat-error" role="alert">{error}</p>}
        <p className="privacy-note">Conversation is stored in the local event database.</p>
      </form>
    </aside>
  );
}
