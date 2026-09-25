import "./styles.css";

function App() {
  return (
    <main className="shell">
      <header className="topbar">
        <div className="brand-mark" aria-hidden="true">A</div>
        <div>
          <p className="eyebrow">Resident Codex session</p>
          <h1>Agentic Factory</h1>
        </div>
        <span className="service-state"><span /> Service ready</span>
      </header>

      <section className="welcome-card" aria-labelledby="welcome-title">
        <div className="welcome-orbit" aria-hidden="true"><span /><span /><span /></div>
        <p className="eyebrow">Your workspace stays close</p>
        <h2 id="welcome-title">A persistent home for Codex work.</h2>
        <p className="welcome-copy">
          This first release keeps one real Codex session running in a local,
          read-only disposable repository. Close the window to keep it resident;
          reopen the tray menu whenever you want to check its output.
        </p>
        <div className="welcome-note">
          <span className="note-icon" aria-hidden="true">↳</span>
          <p>Session output is stored locally and resumes from the event ledger.</p>
        </div>
      </section>

      <footer className="footer">
        <span>Local first · Read only</span>
        <span>Codex App Server</span>
      </footer>
    </main>
  );
}

export default App;
