
(function () {
  "use strict";
  var workflow = {
    request: {
      type: "USER REQUEST", title: "Invite teammates by email", owner: "You", initials: "Y",
      status: "Captured", tone: "done", duration: "09:14", dependencies: "—",
      summary: "A request to add teammate invitations through the existing team settings page.",
      decision: "Single-use links, expiry enforced on the server, and no direct membership writes from the browser.",
      messages: [{from:"Mara",time:"09:14",text:"I’ve recorded your single-use and server-side acceptance constraints in the run brief."}],
      logs: [["09:14:02","user","Request received: add teammate invitation by email."],["09:14:06","manager","Constraints noted: single-use token; server-owned membership writes."]]
    },
    manager: {
      type: "MANAGER PLAN", title: "Mara routes the work", owner: "Mara · manager agent", initials: "M",
      status: "Plan complete", tone: "done", duration: "00:18", dependencies: "User request",
      summary: "Split the work into API and settings UI tasks. The end-to-end test task waits for both builders to finish and share their branches.",
      decision: "Parallelize API and UI. Keep invitation acceptance and membership mutation on the server. Permissions changes require user review; all checks must pass before merge.",
      messages: [{from:"Mara → API + UI",time:"09:15",text:"Build the invitation contract and settings experience in parallel. Please message each other directly when the payload shape is settled."},{from:"Mara → QA",time:"09:15",text:"Wait for both build tasks, then cover acceptance, expiry, and replay."}],
      logs: [["09:14:18","manager","Plan created: API + UI in parallel; tests depend on both."],["09:14:26","manager","Assigned three isolated Codex CLI workspaces."],["09:14:31","manager","Recorded conservative merge and production-watch gates."]]
    },
    api: {
      type: "CODEX CLI · BACKEND", title: "Invitation API", owner: "Ari · Codex CLI", initials: "A",
      status: "Complete", tone: "done", duration: "2m 41s", dependencies: "Manager plan",
      summary: "Added server-owned invite creation and acceptance. Tokens are single-use, expire on schedule, and cannot create duplicate memberships.",
      decision: "Use a hashed, single-use token with server-side expiry validation. Acceptance consumes the token in the same transaction as membership creation.",
      messages: [{from:"Ari → Uma",time:"09:42",text:"Contract is ready: POST /invites returns inviteId, email, expiresAt. Accept endpoint consumes the token atomically; replay returns 410."}],
      logs: [["09:38:02","api","$ codex exec --full-auto 'implement invite API'"],["09:39:11","api","Added create and accept handlers with hashed single-use token."],["09:40:26","api","Acceptance and membership write share one transaction."],["09:42:01","api","Contract shared with UI builder; replay returns 410."],["09:42:33","api","$ pnpm test --filter invitation-api"],["09:43:04","api","<span class='ok'>PASS</span> 14 API tests"]]
    },
    ui: {
      type: "CODEX CLI · FRONTEND", title: "Settings invitation UI", owner: "Uma · Codex CLI", initials: "U",
      status: "Complete", tone: "done", duration: "3m 08s", dependencies: "Manager plan · API contract",
      summary: "Built the invite form and pending-invitation list, including clear expired states and accessible feedback.",
      decision: "Show pending and expired invitations separately. Reuse the existing settings form controls and let the API own all acceptance decisions.",
      messages: [{from:"Uma → Ari",time:"09:39",text:"I can consume inviteId, email, expiresAt. Please confirm acceptance response for already-used links."},{from:"Ari → Uma",time:"09:42",text:"Confirmed: success returns membership; consumed or expired token returns 410. Contract now stable."},{from:"Uma → Mara",time:"09:51",text:"UI branch is ready. Loading, empty, expired and API error states are covered."}],
      logs: [["09:36:40","ui","$ codex exec --full-auto 'build team invite settings UI'"],["09:38:22","ui","Added invite form and pending list states."],["09:39:08","ui","Asked API builder to confirm consumed-token response."],["09:42:19","ui","Contract received; wired success and 410 states."],["09:45:42","ui","Added keyboard and screen-reader status feedback."],["09:49:48","ui","$ pnpm test --filter team-settings"],["09:50:21","ui","<span class='ok'>PASS</span> 22 component tests"]]
    },
    qa: {
      type: "CODEX CLI · VERIFICATION", title: "Invitation flow tests", owner: "Quinn · Codex CLI", initials: "Q",
      status: "Complete", tone: "done", duration: "1m 56s", dependencies: "API + UI branches",
      summary: "Joined both branches and exercised invitation creation, acceptance, expiry, and replay in a browser flow.",
      decision: "Add an explicit replay check: the second acceptance must fail and leave the existing membership unchanged.",
      messages: [{from:"Quinn → Mara",time:"09:58",text:"Both handoffs are green. E2E covers accepted, expired, and replayed links; no membership duplication."}],
      logs: [["09:52:07","system","Both builder branches available; starting integration."],["09:52:14","qa","$ pnpm test:e2e --project chromium invitation-flow"],["09:54:43","qa","Accepted invite creates one membership."],["09:55:26","qa","Expired invite rejected; membership unchanged."],["09:56:18","qa","Replay rejected with 410; duplicate membership absent."],["09:58:05","qa","<span class='ok'>PASS</span> 8/8 browser scenarios"]]
    },
    checks: {
      type: "CI QUALITY GATE", title: "Required checks", owner: "CI · protected branch", initials: "CI",
      status: "8 / 8 passed", tone: "done", duration: "4m 12s", dependencies: "API + UI + tests",
      summary: "Protected-branch checks are all green. The merge gate will re-check required status before accepting the merge.",
      decision: "Every required check must pass on the reviewed commit. A failed or pending check blocks merge and returns the run to Mara for repair.",
      messages: [{from:"CI → Mara",time:"10:01",text:"All required checks passed on commit 8af31c2. Branch protection is satisfied."}],
      logs: [["09:57:02","system","Commit 8af31c2 opened CI checks."],["09:58:12","system","Lint, typecheck, unit, and API checks passed."],["09:59:28","system","Browser E2E and accessibility checks passed."],["10:00:14","system","Build and dependency scan passed."],["10:01:10","system","<span class='ok'>8/8 REQUIRED CHECKS PASSED</span>"]]
    },
    pr: {
      type: "PULL REQUEST · #248", title: "Ready for your review", owner: "Mara · review coordinator", initials: "M",
      status: "Review needed", tone: "review", duration: "10:02", dependencies: "Checks 8/8 · risk review",
      summary: "The feature branch is ready. Invitation permissions are outside the low-risk policy, so the PR awaits your review while required checks remain green.",
      decision: "Auto-merge is armed only for low-risk changes, an approved review, and passing required checks. Any failed check, elevated risk, or requested change pauses the merge.",
      messages: [{from:"Mara → You",time:"10:02",text:"PR #248 is ready. Please review the API transaction and the settings states; the summary and risk notes are attached."}],
      logs: [["10:01:12","system","Pull request #248 created from feat/team-invites."],["10:01:16","system","Risk scan: permissions change · review required."],["10:01:18","system","Auto-merge blocked by conservative risk policy."],["10:02:03","manager","<span class='accent'>PR ready for your review.</span>"]]
    },
    deploy: {
      type: "PRODUCTION RELEASE", title: "Deploy after merge", owner: "Release runner", initials: "R",
      status: "Waiting on PR", tone: "waiting", duration: "Queued", dependencies: "Approved PR #248",
      summary: "After merge, deploy progressively and verify the service health check before marking the release complete.",
      decision: "On a failed production health check, alert the user and wait for direction. Keep rollback information available without performing a rollback automatically.",
      messages: [{from:"Mara → Release runner",time:"queued",text:"Start only after PR #248 merges. Pause rollout if health checks fail."}],
      logs: [["queue","system","Production release is queued behind PR #248."],["queue","system","Progressive rollout and health check are configured."]]
    },
    watch: {
      type: "PRODUCTION MONITOR", title: "Monitor after deploy", owner: "Mara · manager agent", initials: "M",
      status: "Armed", tone: "waiting", duration: "Post-deploy", dependencies: "Healthy production deploy",
      summary: "Once deployed, watch production health and alert Mara if errors or latency cross the agreed threshold.",
      decision: "On an actionable alert, notify the user and wait for a decision before any retry or rollback.",
      messages: [{from:"Monitor → Mara",time:"armed",text:"Monitoring starts after production deploy. On threshold breach: pause, alert and wait."}],
      logs: [["armed","system","Production watch is configured and waiting for deployment."],["armed","system","On failure: stop rollout, alert Mara, await direction."]]
    }
  };

  var selectedId = "qa";
  var inspectorTab = "overview";
  var dockTab = "terminal";
  var viewport = document.getElementById("canvasViewport");
  var world = document.getElementById("canvasWorld");
  var zoom = 0.8;
  var offsetX = 0;
  var offsetY = 0;
  var toastTimer;
  var paused = false;

  function escapeHtml(value) {
    return String(value).replace(/[&<>"']/g, function (char) {
      return {"&":"&amp;","<":"&lt;",">":"&gt;",'"':"&quot;","'":"&#39;"}[char];
    });
  }
  function showToast(message) {
    var toast = document.getElementById("toast");
    toast.textContent = message;
    toast.classList.add("show");
    clearTimeout(toastTimer);
    toastTimer = setTimeout(function () { toast.classList.remove("show"); }, 2300);
  }
  function applyTransform() {
    world.style.transform = "translate(" + offsetX + "px," + offsetY + "px) scale(" + zoom + ")";
    document.getElementById("zoomReadout").textContent = Math.round(zoom * 100) + "%";
  }
  function fitCanvas() {
    var rect = viewport.getBoundingClientRect();
    zoom = Math.min((rect.width - 28) / 1130, (rect.height - 28) / 625, 1);
    zoom = Math.max(.45, zoom);
    offsetX = (rect.width - 1130 * zoom) / 2;
    offsetY = (rect.height - 625 * zoom) / 2;
    applyTransform();
  }
  function setZoom(next, clientX, clientY) {
    var rect = viewport.getBoundingClientRect();
    var cx = typeof clientX === "number" ? clientX - rect.left : rect.width / 2;
    var cy = typeof clientY === "number" ? clientY - rect.top : rect.height / 2;
    var pointX = (cx - offsetX) / zoom;
    var pointY = (cy - offsetY) / zoom;
    zoom = Math.max(.45, Math.min(1.25, next));
    offsetX = cx - pointX * zoom;
    offsetY = cy - pointY * zoom;
    applyTransform();
  }
  function activeData() { return workflow[selectedId]; }
  function statusMarkup(data) {
    var tone = data.tone === "review" ? "review" : data.tone === "waiting" ? "waiting" : "";
    return '<span class="node-status large ' + tone + '"><span class="dot"></span>' + escapeHtml(data.status) + '</span>';
  }
  function renderInspector() {
    var data = activeData();
    document.getElementById("inspectorType").textContent = data.type.split(" · ")[0];
    var html = '<div class="selected-intro">' +
      '<div class="selected-kicker"><span>Selected node</span><span>' + escapeHtml(data.duration) + '</span></div>' +
      '<div class="selected-title">' + escapeHtml(data.title) + '</div>' +
      '<div class="selected-subtitle"><span class="avatar-small">' + escapeHtml(data.initials) + '</span>' + escapeHtml(data.owner) + '</div>' +
      statusMarkup(data) + '</div>' +
      '<div class="inspector-tabs" role="tablist">' +
      '<button class="inspector-tab ' + (inspectorTab === "overview" ? "active" : "") + '" data-inspector="overview">Overview</button>' +
      '<button class="inspector-tab ' + (inspectorTab === "cli" ? "active" : "") + '" data-inspector="cli">CLI</button>' +
      '<button class="inspector-tab ' + (inspectorTab === "messages" ? "active" : "") + '" data-inspector="messages">Messages</button>' +
      '<button class="inspector-tab ' + (inspectorTab === "decisions" ? "active" : "") + '" data-inspector="decisions">Decisions</button></div>';
    if (inspectorTab === "overview") {
      html += '<div class="info-section"><div class="section-label">Work summary</div><div class="info-copy">' + escapeHtml(data.summary) + '</div></div>' +
        '<div class="info-section"><div class="section-label">Run details</div><div class="info-card info-grid"><div class="info-cell"><div class="label">Depends on</div><div class="value">' + escapeHtml(data.dependencies) + '</div></div><div class="info-cell"><div class="label">Elapsed / state</div><div class="value mono">' + escapeHtml(data.duration) + '</div></div></div></div>' +
        '<div class="info-section"><div class="section-label">Decision recorded</div><div class="decision-card"><div class="decision-title">Guardrail</div><div class="decision-copy">' + escapeHtml(data.decision) + '</div></div></div>' +
        '<button class="inspector-cta" data-open-cli="true">Open this agent’s CLI output ↓</button>';
    } else if (inspectorTab === "cli") {
      if (data.logs.length) {
        html += '<div class="info-section"><div class="section-label">Simulated CLI stream</div><div class="code-card">';
        data.logs.forEach(function (row) {
          html += '<div><span class="dim">[' + escapeHtml(row[0]) + ']</span> <span class="prompt">' + escapeHtml(row[1]) + '$</span> ' + row[2] + '</div>';
        });
        html += '</div></div><div class="empty-note">Output is simulated for this prototype. No repository or CLI process is connected.</div>';
      }
    } else if (inspectorTab === "messages") {
      html += '<div class="info-section"><div class="section-label">Agent communication</div>';
      data.messages.forEach(function (message) {
        html += '<div class="message-card"><div class="message-card-head"><strong>' + escapeHtml(message.from) + '</strong><span>' + escapeHtml(message.time) + '</span></div><p>' + escapeHtml(message.text) + '</p></div>';
      });
      html += '</div><div class="empty-note">Builders can hand work to one another. You continue to message Mara only.</div>';
    } else {
      html += '<div class="info-section"><div class="section-label">Decision log</div><div class="decision-card"><div class="decision-title">Recorded choice</div><div class="decision-copy">' + escapeHtml(data.decision) + '</div></div></div>' +
        '<div class="info-section"><div class="section-label">Gate behavior</div><div class="info-card info-copy">' +
        (selectedId === "pr" ? 'This permissions change requires your review. Every required check must pass before merge; only eligible low-risk changes can auto-merge.' :
         selectedId === "deploy" || selectedId === "watch" ? 'Deployment waits for merge. A production failure alerts you and waits for your direction; no automatic rollback.' :
         'The manager keeps the user constraints visible to builders and verifies dependent work before the PR gate.') +
        '</div></div>';
    }
    document.getElementById("inspectorBody").innerHTML = html;
    document.querySelectorAll(".inspector-tab").forEach(function (tab) {
      tab.addEventListener("click", function () {
        inspectorTab = tab.dataset.inspector;
        renderInspector();
      });
    });
    var open = document.querySelector("[data-open-cli]");
    if (open) open.addEventListener("click", function () { inspectorTab = "cli"; renderInspector(); });
  }
  function selectNode(id) {
    if (!workflow[id]) return;
    selectedId = id;
    document.querySelectorAll(".node, .agent-window").forEach(function (node) {
      node.classList.toggle("selected", node.dataset.node === id);
      node.setAttribute("aria-pressed", node.dataset.node === id ? "true" : "false");
    });
    inspectorTab = "overview";
    renderInspector();
    if (dockTab === "terminal") renderDock();
  }
  function renderLogs(rows) {
    return '<div class="log-list">' + rows.map(function (row) {
      var sourceClass = row[1] === "ui" ? "ui" : row[1] === "api" ? "api" : row[1] === "system" ? "system" : "";
      return '<div class="log-line"><span class="log-time">' + escapeHtml(row[0]) + '</span><span class="log-source ' + sourceClass + '">' + escapeHtml(row[1]) + '</span><span class="log-text">' + row[2] + '</span></div>';
    }).join("") + '</div>';
  }
  function renderDock() {
    var body = document.getElementById("dockBody");
    document.querySelectorAll(".dock-tab").forEach(function (tab) {
      tab.classList.toggle("active", tab.dataset.dock === dockTab);
    });
    if (dockTab === "terminal") {
      document.getElementById("dockStatusText").textContent = paused ? "OUTPUT PAUSED · SIMULATED" : "STREAMING SIMULATED OUTPUT";
      body.innerHTML = renderLogs(activeData().logs);
    } else if (dockTab === "handoffs") {
      document.getElementById("dockStatusText").textContent = "BUILDER MESSAGES · SIMULATED";
      body.innerHTML = '<div class="handoff-list">' +
        '<div class="handoff-row"><span class="handoff-arrow">↗</span><span><b style="color:#d1c5ff">Mara → Ari + Uma</b> · API and settings UI can proceed in parallel. Share the contract directly when it is stable.</span><span class="handoff-time">09:15</span></div>' +
        '<div class="handoff-row"><span class="handoff-arrow">↔</span><span><b style="color:#d1c5ff">Ari ↔ Uma</b> · Invite payload confirmed; consumed or expired token returns <code>410</code>. UI builder continues against the shared contract.</span><span class="handoff-time">09:42</span></div>' +
        '<div class="handoff-row"><span class="handoff-arrow">↘</span><span><b style="color:#d1c5ff">Ari + Uma → Quinn</b> · Both branches ready. E2E builder joined API and UI branches and completed the acceptance, expiry and replay flows.</span><span class="handoff-time">09:52</span></div></div>';
    } else {
      document.getElementById("dockStatusText").textContent = "MERGE GATES · SIMULATED";
      body.innerHTML = '<div class="gate-list">' +
        '<div class="gate-row"><span class="gate-check" style="color:var(--amber);background:var(--amber-dim)">!</span><span>Invitation permissions require your review</span><span class="gate-note">awaiting</span></div>' +
        '<div class="gate-row"><span class="gate-check">✓</span><span>All protected branch checks pass</span><span class="gate-note">8 / 8 green</span></div>' +
        '<div class="gate-row"><span class="gate-check" style="color:var(--amber);background:var(--amber-dim)">!</span><span>Auto-merge excluded by risk policy</span><span class="gate-note">permissions</span></div>' +
        '<div class="gate-row"><span class="gate-check" style="color:var(--amber);background:var(--amber-dim)">…</span><span>Wait for your review; block merge if any check fails</span><span class="gate-note">held</span></div></div>';
    }
  }

  document.querySelectorAll(".node, .agent-window").forEach(function (node) {
    node.addEventListener("click", function (event) { event.stopPropagation(); selectNode(node.dataset.node); });
    node.addEventListener("keydown", function (event) {
      if (event.key === "Enter" || event.key === " ") { event.preventDefault(); selectNode(node.dataset.node); }
    });
  });
  document.querySelectorAll(".dock-tab").forEach(function (tab) {
    tab.addEventListener("click", function () { dockTab = tab.dataset.dock; renderDock(); });
  });
  document.getElementById("zoomIn").addEventListener("click", function () { setZoom(zoom * 1.12); });
  document.getElementById("zoomOut").addEventListener("click", function () { setZoom(zoom / 1.12); });
  document.getElementById("fitCanvas").addEventListener("click", fitCanvas);
  window.addEventListener("resize", fitCanvas);
  document.getElementById("panelToggle").addEventListener("click", function () {
    var focused = document.getElementById("appShell").classList.toggle("canvas-focus");
    this.textContent = focused ? "Show manager + details" : "Focus CLI canvas";
    fitCanvas();
  });

  var replayButton = document.getElementById("replayCoordination");
  var coordinationEvent = document.getElementById("coordinationEvent");
  var managerTerminal = document.getElementById("managerTerminal");
  var settledManagerOutput = managerTerminal.innerHTML;
  function wait(ms) { return new Promise(function (resolve) { setTimeout(resolve, ms); }); }
  function announceCoordination(message) {
    coordinationEvent.textContent = message;
    coordinationEvent.classList.add("visible");
  }
  function spawnAgent(id) {
    var windowNode = document.querySelector(".agent-window." + id + "-cli");
    windowNode.classList.remove("unspawned");
    windowNode.classList.add("spawning");
    setTimeout(function () { windowNode.classList.remove("spawning"); }, 500);
  }
  function passMessage(routeId, message) {
    return new Promise(function (resolve) {
      var route = document.getElementById(routeId);
      var packet = document.getElementById("messagePacket");
      var length = route.getTotalLength();
      var started;
      announceCoordination(message);
      route.classList.add("active");
      packet.classList.add("active");
      function frame(time) {
        if (!started) started = time;
        var progress = Math.min((time - started) / 1250, 1);
        var point = route.getPointAtLength(progress * length);
        packet.setAttribute("cx", point.x);
        packet.setAttribute("cy", point.y);
        if (progress < 1) requestAnimationFrame(frame);
        else { route.classList.remove("active"); packet.classList.remove("active"); resolve(); }
      }
      requestAnimationFrame(frame);
    });
  }
  replayButton.addEventListener("click", async function () {
    replayButton.disabled = true;
    replayButton.textContent = "Replaying…";
    document.querySelectorAll(".agent-window:not(.manager-cli)").forEach(function (node) { node.classList.add("unspawned"); });
    selectNode("manager");
    managerTerminal.textContent = '$ codex exec "coordinate run #184"\nPlanning API + UI slices…\nCreating agent worktrees…';
    announceCoordination("Mara creates an isolated worktree and spawns Ari · API builder");
    await wait(1000);
    spawnAgent("api");
    managerTerminal.textContent += '\n✓ Ari spawned in agent/api-invites';
    await wait(850);
    announceCoordination("Mara creates a second worktree and spawns Uma · UI builder");
    await wait(850);
    spawnAgent("ui");
    managerTerminal.textContent += '\n✓ Uma spawned in agent/ui-invites';
    await wait(650);
    await passMessage("routeApiUi", "Ari → Uma: invitation payload and expiry contract");
    await passMessage("routeUiManager", "Uma → Mara: UI is ready against the agreed API contract");
    announceCoordination("Mara spawns Quinn after both builder handoffs are ready");
    await wait(950);
    spawnAgent("qa");
    managerTerminal.textContent += '\n✓ Quinn spawned after builder handoffs';
    await wait(550);
    await passMessage("routeManagerQa", "Mara → Quinn: verify API + UI together, including replay and expiry");
    announceCoordination("All Codex CLI sessions are visible · PR awaits your review");
    managerTerminal.innerHTML = settledManagerOutput;
    await wait(2000);
    coordinationEvent.classList.remove("visible");
    replayButton.disabled = false;
    replayButton.textContent = "▶ Replay agent coordination";
  });

  var pointer = null;
  viewport.addEventListener("pointerdown", function (event) {
    if (event.target.closest(".node, .agent-window")) return;
    pointer = {x:event.clientX, y:event.clientY, ox:offsetX, oy:offsetY, id:event.pointerId};
    viewport.setPointerCapture(event.pointerId);
    viewport.classList.add("dragging");
  });
  viewport.addEventListener("pointermove", function (event) {
    if (!pointer || pointer.id !== event.pointerId) return;
    offsetX = pointer.ox + event.clientX - pointer.x;
    offsetY = pointer.oy + event.clientY - pointer.y;
    applyTransform();
  });
  function endPan(event) {
    if (pointer && pointer.id === event.pointerId) {
      pointer = null;
      viewport.classList.remove("dragging");
    }
  }
  viewport.addEventListener("pointerup", endPan);
  viewport.addEventListener("pointercancel", endPan);
  viewport.addEventListener("wheel", function (event) {
    event.preventDefault();
    setZoom(zoom * (event.deltaY < 0 ? 1.08 : 1 / 1.08), event.clientX, event.clientY);
  }, {passive:false});

  document.getElementById("pauseButton").addEventListener("click", function () {
    paused = !paused;
    this.textContent = paused ? "Resume run" : "Pause run";
    var state = document.getElementById("runState");
    state.innerHTML = paused ? '<span class="dot" style="color:var(--amber)"></span> RUN PAUSED' : '<span class="dot"></span> RUN ACTIVE';
    state.style.color = paused ? "var(--amber)" : "var(--mint)";
    state.style.borderColor = paused ? "rgba(240,196,119,.25)" : "rgba(130,226,189,.2)";
    renderDock();
    showToast(paused ? "Simulation paused" : "Simulation resumed");
  });

  document.getElementById("chatForm").addEventListener("submit", function (event) {
    event.preventDefault();
    var input = document.getElementById("chatInput");
    var text = input.value.trim();
    if (!text) return;
    var feed = document.getElementById("chatFeed");
    var user = document.createElement("div");
    user.className = "message user";
    user.innerHTML = '<div class="message-meta"><span>now</span><strong>You</strong></div><div class="message-bubble"></div>';
    user.querySelector(".message-bubble").textContent = text;
    feed.appendChild(user);
    var reply = document.createElement("div");
    reply.className = "message";
    reply.innerHTML = '<div class="message-meta"><strong>Mara</strong><span>now</span></div><div class="message-bubble"></div>';
    reply.querySelector(".message-bubble").textContent = "Noted for this simulated run. I’ll keep that constraint visible to the builders and preserve the existing review and deployment gates.";
    feed.appendChild(reply);
    feed.scrollTop = feed.scrollHeight;
    input.value = "";
  });
  document.getElementById("chatInfo").addEventListener("click", function () {
    showToast("Your messages go to Mara; builder-to-builder messages stay visible in the run.");
  });
  document.getElementById("copyNode").addEventListener("click", function () {
    showToast("Node reference: " + selectedId + " · simulated workspace");
  });

  var workspaceOverlay = document.getElementById("workspaceOverlay");
  function setWorkspaceOpen(open) {
    workspaceOverlay.classList.toggle("open", open);
    workspaceOverlay.setAttribute("aria-hidden", open ? "false" : "true");
  }
  document.getElementById("allRunsButton").addEventListener("click", function () { setWorkspaceOpen(true); });
  document.getElementById("worktreesButton").addEventListener("click", function () { setWorkspaceOpen(true); });
  document.getElementById("closeWorkspace").addEventListener("click", function () { setWorkspaceOpen(false); });
  document.querySelector('[data-open-run="184"]').addEventListener("click", function () { setWorkspaceOpen(false); });
  document.querySelectorAll("[data-demo-run]").forEach(function (button) {
    button.addEventListener("click", function () { showToast("Run #" + button.dataset.demoRun + " is an illustrative linked task in this prototype."); });
  });
  document.querySelectorAll("[data-worktree-action]").forEach(function (button) {
    button.addEventListener("click", function () {
      var action = button.dataset.worktreeAction;
      var name = button.dataset.worktree || "new task";
      showToast(action === "archive" ? "Simulation: check for uncommitted work before archiving " + name : action === "create" ? "Simulation: manager creates an isolated worktree for a new task" : "Simulation: inspect " + name + " status, branch and changed files");
    });
  });
  document.addEventListener("keydown", function (event) { if (event.key === "Escape") setWorkspaceOpen(false); });

  document.getElementById("dockBody").addEventListener("click", function (event) {
    if (event.target.closest("[data-open-node]")) selectNode(event.target.closest("[data-open-node]").dataset.openNode);
  });

  selectNode(selectedId);
  renderDock();
  fitCanvas();
})();
