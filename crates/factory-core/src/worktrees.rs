use crate::{
    ledger::Ledger,
    model::{
        AgentId, ArchiveOutcome, RecoveryIssue, RepoId, RunId, Worktree, WorktreeId, WorktreeRole,
        WorktreeState, WorktreeStatus,
    },
    repositories::{path_to_text, RepositoryRegistry},
};
use anyhow::{anyhow, bail, Context, Result};
use rusqlite::{params, OptionalExtension, TransactionBehavior};
#[cfg(unix)]
use std::os::{fd::AsRawFd, unix::fs::OpenOptionsExt};
use std::{
    ffi::OsString,
    fs::{self, File, OpenOptions},
    path::{Component, Path, PathBuf},
    process::{Command, Output},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone)]
pub struct WorktreeManager {
    ledger: Ledger,
    root: PathBuf,
}

struct WorktreeLock(File);

#[cfg(unix)]
impl Drop for WorktreeLock {
    fn drop(&mut self) {
        let _ = unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}

impl WorktreeManager {
    pub fn new(ledger: Ledger, root: PathBuf) -> Result<Self> {
        fs::create_dir_all(&root)
            .with_context(|| format!("creating worktree root at {}", root.display()))?;
        let root = root
            .canonicalize()
            .with_context(|| format!("canonicalizing worktree root at {}", root.display()))?;
        path_to_text(&root)?;
        Ok(Self { ledger, root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Runs an integration or inspection operation while holding the worktree's cross-process
    /// lock and after confirming that its registered Git identity is still safe.
    pub fn with_exclusive_worktree<T>(
        &self,
        worktree_id: WorktreeId,
        operation: impl FnOnce(&Worktree, WorktreeStatus) -> Result<T>,
    ) -> Result<T> {
        let Some(_lock) = self.try_worktree_lock(worktree_id)? else {
            bail!("worktree has another operation in progress");
        };
        let worktree = self.get_worktree(worktree_id)?;
        if worktree.state != WorktreeState::Active {
            bail!("worktree is not active");
        }
        let status = self.inspect_worktree_locked(worktree_id)?;
        match status {
            WorktreeStatus::Clean | WorktreeStatus::Dirty => operation(&worktree, status),
            _ => bail!("worktree requires manager recovery"),
        }
    }

    pub fn create_run_worktree(
        &self,
        repo_id: RepoId,
        run_id: RunId,
        base_sha: &str,
    ) -> Result<Worktree> {
        self.create_worktree(repo_id, run_id, WorktreeRole::Integration, base_sha)
    }

    pub fn create_agent_worktree(
        &self,
        run_id: RunId,
        agent_id: AgentId,
        base_sha: &str,
    ) -> Result<Worktree> {
        let integration = self.integration_worktree(run_id)?;
        if integration.state != WorktreeState::Active {
            bail!("run integration worktree is not active");
        }
        if integration.base_sha != base_sha {
            bail!("agent base SHA must match the run's recorded base SHA");
        }
        self.create_worktree(
            integration.repo_id,
            run_id,
            WorktreeRole::Agent(agent_id),
            base_sha,
        )
    }

    /// Moves an unused clean agent worktree to the current integration commit. This makes a
    /// dependent slice see the already integrated outputs of its prerequisites. The method only
    /// resets a branch when its HEAD still equals its recorded base; a branch with commits is
    /// preserved for manager review.
    pub fn advance_clean_agent_worktree_to_integration(
        &self,
        agent_worktree_id: WorktreeId,
    ) -> Result<Worktree> {
        let agent = self.get_worktree(agent_worktree_id)?;
        if !matches!(agent.role, WorktreeRole::Agent(_)) {
            bail!("only an agent worktree can be advanced to integration");
        }
        let integration = self.integration_worktree(agent.run_id)?;
        if integration.repo_id != agent.repo_id {
            bail!("agent and integration worktrees belong to different repositories");
        }
        let (first, second) = if agent.id < integration.id {
            (agent.id, integration.id)
        } else {
            (integration.id, agent.id)
        };
        let Some(_first_lock) = self.try_worktree_lock(first)? else {
            bail!("worktree has another operation in progress");
        };
        let Some(_second_lock) = self.try_worktree_lock(second)? else {
            bail!("worktree has another operation in progress");
        };
        let agent = self.get_worktree(agent_worktree_id)?;
        let integration = self.get_worktree(integration.id)?;
        if agent.state != WorktreeState::Active || integration.state != WorktreeState::Active {
            bail!("agent and integration worktrees must both be active");
        }
        if self.inspect_worktree_locked(agent.id)? != WorktreeStatus::Clean {
            bail!("agent worktree is not clean enough to advance");
        }
        if self.inspect_worktree_locked(integration.id)? != WorktreeStatus::Clean {
            bail!("integration worktree is not clean enough to advance an agent");
        }
        let repository = RepositoryRegistry::new(self.ledger.clone()).get(agent.repo_id)?;
        let integration_head = run_git(
            &integration.path,
            [OsString::from("rev-parse"), OsString::from("HEAD")],
        )?;
        let integration_head = resolve_full_commit(&repository.canonical_root, &integration_head)?;
        if integration_head != agent.base_sha {
            let ancestor = run_git(
                &repository.canonical_root,
                [
                    OsString::from("merge-base"),
                    OsString::from("--is-ancestor"),
                    OsString::from(&integration.base_sha),
                    OsString::from(&integration_head),
                ],
            );
            if ancestor.is_err() {
                bail!("current integration commit no longer descends from the run base SHA");
            }
        }
        let agent_head = run_git(
            &agent.path,
            [OsString::from("rev-parse"), OsString::from("HEAD")],
        )?;
        if agent_head == agent.base_sha {
            if integration_head != agent_head {
                run_git(
                    &agent.path,
                    [
                        OsString::from("reset"),
                        OsString::from("--hard"),
                        OsString::from(&integration_head),
                    ],
                )?;
            }
        } else if agent_head != integration_head {
            bail!("agent branch already contains worker commits; automatic base advancement is unsafe");
        }
        self.ledger.with_connection(|connection| {
            connection.execute(
                "UPDATE worktrees SET base_sha = ?2 WHERE id = ?1 AND state = 'active'",
                params![agent.id.to_string(), integration_head],
            )?;
            Ok(())
        })?;
        self.get_worktree(agent.id)
    }

    pub fn inspect_worktree(&self, worktree_id: WorktreeId) -> Result<WorktreeStatus> {
        let Some(lock) = self.try_worktree_lock(worktree_id)? else {
            return Ok(WorktreeStatus::Busy);
        };
        let status = self.inspect_worktree_locked(worktree_id);
        drop(lock);
        status
    }

    fn inspect_worktree_locked(&self, worktree_id: WorktreeId) -> Result<WorktreeStatus> {
        let worktree = self.get_worktree(worktree_id)?;
        let archived = match worktree.state {
            WorktreeState::Creating => return Ok(WorktreeStatus::Creating),
            WorktreeState::Archived => true,
            WorktreeState::RecoveryRequired => return Ok(WorktreeStatus::RecoveryRequired),
            WorktreeState::Active => false,
        };

        if path_has_symlink(&self.root, &worktree.path)? {
            self.require_recovery(
                worktree_id,
                "symlinked_worktree",
                "worktree path traverses a symbolic link",
            )?;
            return Ok(WorktreeStatus::Unsafe);
        }
        if !worktree.path.exists() {
            self.require_recovery(
                worktree_id,
                "missing_worktree",
                "registered worktree path is missing",
            )?;
            return Ok(WorktreeStatus::Missing);
        }

        let canonical_path = match worktree.path.canonicalize() {
            Ok(path) if path.starts_with(&self.root) => path,
            _ => {
                self.require_recovery(
                    worktree_id,
                    "moved_worktree",
                    "worktree resolved outside the configured worktree root",
                )?;
                return Ok(WorktreeStatus::Unsafe);
            }
        };
        let repository = RepositoryRegistry::new(self.ledger.clone()).get(worktree.repo_id)?;
        if self
            .ensure_worktree_identity(&repository.canonical_root, &canonical_path, &worktree)
            .is_err()
        {
            self.require_recovery(
                worktree_id,
                "git_identity_mismatch",
                "worktree Git identity does not match its registered repository and branch",
            )?;
            return Ok(WorktreeStatus::Unsafe);
        }

        let status = run_git(
            &canonical_path,
            [
                OsString::from("status"),
                OsString::from("--porcelain=v1"),
                OsString::from("--untracked-files=all"),
                OsString::from("--ignored=matching"),
            ],
        );
        match status {
            Ok(status) if status.is_empty() && archived => Ok(WorktreeStatus::Archived),
            Ok(status) if status.is_empty() => Ok(WorktreeStatus::Clean),
            Ok(_) if archived => {
                self.require_recovery(
                    worktree_id,
                    "archived_worktree_modified",
                    "archived worktree contains changed, untracked, or ignored files",
                )?;
                Ok(WorktreeStatus::RecoveryRequired)
            }
            Ok(_) => Ok(WorktreeStatus::Dirty),
            Err(error) => {
                self.require_recovery(
                    worktree_id,
                    "git_inspection_failed",
                    "Git could not inspect the registered worktree",
                )?;
                Err(error)
            }
        }
    }

    pub fn archive_worktree(&self, worktree_id: WorktreeId) -> Result<ArchiveOutcome> {
        let Some(lock) = self.try_worktree_lock(worktree_id)? else {
            bail!("another worktree operation is already in progress");
        };
        let outcome = self.archive_worktree_locked(worktree_id);
        drop(lock);
        outcome
    }

    fn archive_worktree_locked(&self, worktree_id: WorktreeId) -> Result<ArchiveOutcome> {
        let worktree = self.get_worktree(worktree_id)?;
        match self.inspect_worktree_locked(worktree_id)? {
            WorktreeStatus::Dirty => {
                self.record_issue(
                    worktree_id,
                    "dirty_archive_refused",
                    "archive refused because the worktree contains tracked, untracked, or ignored files",
                )?;
                return Ok(ArchiveOutcome::RefusedDirty);
            }
            WorktreeStatus::Unsafe | WorktreeStatus::RecoveryRequired => {
                return Ok(ArchiveOutcome::RefusedUnsafe)
            }
            WorktreeStatus::Missing => return Ok(ArchiveOutcome::Missing),
            WorktreeStatus::Archived => return Ok(ArchiveOutcome::Archived),
            WorktreeStatus::Creating => bail!("worktree creation has not completed"),
            WorktreeStatus::Busy => bail!("another worktree operation is already in progress"),
            WorktreeStatus::Clean => {}
        }

        let archive_path = self.root.join("archive").join(worktree.id.to_string());
        if archive_path.exists() || path_has_symlink(&self.root, &archive_path)? {
            self.require_recovery(
                worktree_id,
                "archive_destination_conflict",
                "archive destination already exists or is unsafe",
            )?;
            return Ok(ArchiveOutcome::RefusedUnsafe);
        }
        fs::create_dir_all(
            archive_path
                .parent()
                .ok_or_else(|| anyhow!("archive path has no parent"))?,
        )?;
        let repository = RepositoryRegistry::new(self.ledger.clone()).get(worktree.repo_id)?;
        run_git(
            &repository.canonical_root,
            [
                OsString::from("worktree"),
                OsString::from("move"),
                path_to_text(&worktree.path)?.into(),
                path_to_text(&archive_path)?.into(),
            ],
        )?;

        let mut archived = worktree;
        archived.path = archive_path;
        archived.state = WorktreeState::Archived;
        self.update_worktree(&archived)?;
        self.resolve_issues(worktree_id)?;
        Ok(ArchiveOutcome::Archived)
    }

    pub fn claim_file_scope(&self, run_id: RunId, agent_id: AgentId, paths: &[&str]) -> Result<()> {
        let agent_worktree = self.find_agent_worktree(run_id, agent_id)?;
        if agent_worktree.state != WorktreeState::Active {
            bail!("agent must have an active worktree before claiming files");
        }
        match self.inspect_worktree(agent_worktree.id)? {
            WorktreeStatus::Clean | WorktreeStatus::Dirty => {}
            WorktreeStatus::Archived => bail!("agent worktree has been archived"),
            WorktreeStatus::Creating => bail!("agent worktree creation has not completed"),
            WorktreeStatus::Busy => bail!("agent worktree has another operation in progress"),
            WorktreeStatus::Missing | WorktreeStatus::Unsafe | WorktreeStatus::RecoveryRequired => {
                bail!("agent worktree requires manager recovery")
            }
        }
        let normalized = paths
            .iter()
            .map(|path| normalize_scope_path(path))
            .collect::<Result<Vec<_>>>()?;
        if normalized.is_empty() {
            bail!("at least one file path must be claimed");
        }

        self.ledger.with_connection(|connection| {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let mut statement = transaction
                .prepare("SELECT agent_id, path FROM file_reservations WHERE run_id = ?1")?;
            let rows = statement.query_map([run_id.to_string()], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            let existing = rows.collect::<rusqlite::Result<Vec<_>>>()?;
            drop(statement);

            for path in &normalized {
                for (owner, claimed_path) in &existing {
                    if owner != &agent_id.to_string() && scopes_overlap(path, claimed_path) {
                        bail!("file scope '{path}' overlaps a scope owned by agent {owner}");
                    }
                }
            }
            for path in &normalized {
                transaction.execute(
                    "INSERT INTO file_reservations(run_id, agent_id, path)
                     VALUES (?1, ?2, ?3) ON CONFLICT DO NOTHING",
                    params![run_id.to_string(), agent_id.to_string(), path],
                )?;
            }
            transaction.commit()?;
            Ok(())
        })
    }

    pub fn release_file_scope(&self, run_id: RunId, agent_id: AgentId) -> Result<()> {
        self.ledger.with_connection(|connection| {
            connection.execute(
                "DELETE FROM file_reservations WHERE run_id = ?1 AND agent_id = ?2",
                params![run_id.to_string(), agent_id.to_string()],
            )?;
            Ok(())
        })
    }

    pub fn ensure_file_scope_available(&self, run_id: RunId, paths: &[String]) -> Result<()> {
        let normalized = paths
            .iter()
            .map(|path| normalize_scope_path(path))
            .collect::<Result<Vec<_>>>()?;
        if normalized.is_empty() {
            bail!("at least one file path must be checked");
        }
        self.ledger.with_connection(|connection| {
            let mut statement = connection
                .prepare("SELECT agent_id, path FROM file_reservations WHERE run_id = ?1")?;
            let rows = statement.query_map([run_id.to_string()], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            let existing = rows.collect::<rusqlite::Result<Vec<_>>>()?;
            for path in &normalized {
                for (owner, claimed_path) in &existing {
                    if scopes_overlap(path, claimed_path) {
                        bail!("file scope '{path}' overlaps a scope owned by agent {owner}");
                    }
                }
            }
            Ok(())
        })
    }

    pub fn recovery_issues(&self) -> Result<Vec<RecoveryIssue>> {
        self.ledger.with_connection(|connection| {
            let mut statement = connection.prepare(
                "SELECT id, worktree_id, kind, detail, recorded_at_ms, resolved_at_ms
                 FROM recovery_issues WHERE resolved_at_ms IS NULL
                 ORDER BY recorded_at_ms, id",
            )?;
            let rows = statement.query_map([], recovery_issue_from_row)?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .context("reading unresolved worktree recovery issues")
        })
    }

    /// Call exactly once during service startup, before accepting worktree creation requests.
    pub fn reconcile(&self) -> Result<Vec<RecoveryIssue>> {
        let worktrees = self.list_worktrees()?;
        for worktree in worktrees {
            let Some(lock) = self.try_worktree_lock(worktree.id)? else {
                continue;
            };
            let result = self.reconcile_worktree_locked(&worktree);
            drop(lock);
            result?;
        }
        self.recovery_issues()
    }

    fn reconcile_worktree_locked(&self, worktree: &Worktree) -> Result<()> {
        match worktree.state {
            WorktreeState::Active => {
                let _ = self.inspect_worktree_locked(worktree.id)?;
            }
            WorktreeState::Creating => {
                if worktree.path.exists() {
                    let repository =
                        RepositoryRegistry::new(self.ledger.clone()).get(worktree.repo_id)?;
                    if self
                        .ensure_worktree_identity(
                            &repository.canonical_root,
                            &worktree.path,
                            &worktree,
                        )
                        .is_ok()
                    {
                        let mut active = worktree.clone();
                        active.state = WorktreeState::Active;
                        self.update_worktree(&active)?;
                    } else {
                        self.require_recovery(
                            worktree.id,
                            "interrupted_creation_unsafe",
                            "interrupted worktree creation left an unverified directory",
                        )?;
                    }
                } else {
                    self.require_recovery(
                        worktree.id,
                        "interrupted_creation_missing",
                        "worktree creation stopped before the Git worktree was established",
                    )?;
                }
            }
            WorktreeState::Archived => {
                let _ = self.inspect_worktree_locked(worktree.id)?;
            }
            WorktreeState::RecoveryRequired => {}
        }
        Ok(())
    }

    #[cfg(unix)]
    fn try_worktree_lock(&self, id: WorktreeId) -> Result<Option<WorktreeLock>> {
        let lock_root = self.root.join(".locks");
        if path_has_symlink(&self.root, &lock_root)? {
            bail!("worktree lock directory is unsafe");
        }
        match fs::create_dir(&lock_root) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
        if path_has_symlink(&self.root, &lock_root)? {
            bail!("worktree lock directory is unsafe");
        }
        let lock_path = lock_root.join(format!("{id}.lock"));
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&lock_path)?;
        let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if result == 0 {
            return Ok(Some(WorktreeLock(file)));
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::EWOULDBLOCK) {
            Ok(None)
        } else {
            Err(error.into())
        }
    }

    #[cfg(not(unix))]
    fn try_worktree_lock(&self, _id: WorktreeId) -> Result<Option<WorktreeLock>> {
        bail!("safe worktree locking requires a Unix host")
    }

    fn create_worktree(
        &self,
        repo_id: RepoId,
        run_id: RunId,
        role: WorktreeRole,
        base_sha: &str,
    ) -> Result<Worktree> {
        let repository = RepositoryRegistry::new(self.ledger.clone()).get(repo_id)?;
        let base_sha = resolve_full_commit(&repository.canonical_root, base_sha)?;
        if let Some(existing) = self.find_worktree_by_run_role(run_id, &role.key())? {
            if existing.repo_id == repo_id && existing.base_sha == base_sha {
                match self.inspect_worktree(existing.id)? {
                    WorktreeStatus::Clean | WorktreeStatus::Dirty => return Ok(existing),
                    WorktreeStatus::Creating
                    | WorktreeStatus::Busy
                    | WorktreeStatus::Missing
                    | WorktreeStatus::Unsafe
                    | WorktreeStatus::Archived
                    | WorktreeStatus::RecoveryRequired => {
                        bail!("existing worktree requires manager recovery")
                    }
                }
            }
            bail!("run worktree already exists with a different repository or base SHA");
        }

        if self.root.starts_with(&repository.canonical_root) {
            bail!("worktree root must be outside the registered repository");
        }
        let (branch_name, path, agent_id, role_kind) = match role {
            WorktreeRole::Integration => (
                format!("factory/run-{run_id}"),
                self.root
                    .join(repo_id.to_string())
                    .join(run_id.to_string())
                    .join("integration"),
                None,
                "integration",
            ),
            WorktreeRole::Agent(agent_id) => (
                format!("factory/agent-{run_id}-{agent_id}"),
                self.root
                    .join(repo_id.to_string())
                    .join(run_id.to_string())
                    .join("agents")
                    .join(agent_id.to_string()),
                Some(agent_id),
                "agent",
            ),
        };
        path_to_text(&path)?;
        if path.exists() || path_has_symlink(&self.root, &path)? {
            bail!("worktree destination already exists or is unsafe");
        }
        let mut worktree = Worktree {
            id: WorktreeId::new(),
            repo_id,
            run_id,
            role: match agent_id {
                Some(agent_id) => WorktreeRole::Agent(agent_id),
                None => WorktreeRole::Integration,
            },
            base_sha: base_sha.clone(),
            branch_name,
            path,
            state: WorktreeState::Creating,
            created_at_ms: now_ms(),
        };
        let Some(creation_lock) = self.try_worktree_lock(worktree.id)? else {
            bail!("another process is creating this worktree");
        };
        self.insert_worktree(&worktree, role_kind, agent_id)?;

        let parent = worktree
            .path
            .parent()
            .ok_or_else(|| anyhow!("worktree path has no parent"))?;
        if let Err(error) = fs::create_dir_all(parent) {
            self.require_recovery(
                worktree.id,
                "worktree_parent_creation_failed",
                "could not prepare the managed worktree directory",
            )?;
            return Err(error.into());
        }
        let added = run_git(
            &repository.canonical_root,
            [
                OsString::from("worktree"),
                OsString::from("add"),
                OsString::from("-b"),
                OsString::from(&worktree.branch_name),
                OsString::from(path_to_text(&worktree.path)?),
                OsString::from(&worktree.base_sha),
            ],
        );
        if let Err(error) = added {
            self.require_recovery(
                worktree.id,
                "worktree_creation_failed",
                "Git failed to create the worktree; the directory and branch need inspection",
            )?;
            return Err(error);
        }
        if let Err(error) =
            self.ensure_worktree_identity(&repository.canonical_root, &worktree.path, &worktree)
        {
            self.require_recovery(
                worktree.id,
                "new_worktree_verification_failed",
                "new Git worktree failed repository and branch identity checks",
            )?;
            return Err(error.context("verifying the newly created Git worktree"));
        }
        worktree.state = WorktreeState::Active;
        self.update_worktree(&worktree)?;
        drop(creation_lock);
        Ok(worktree)
    }

    fn ensure_worktree_identity(
        &self,
        repository_root: &Path,
        path: &Path,
        worktree: &Worktree,
    ) -> Result<()> {
        if path_has_symlink(&self.root, path)? {
            bail!("worktree path contains a symbolic link");
        }
        let canonical_path = path
            .canonicalize()
            .with_context(|| format!("canonicalizing worktree at {}", path.display()))?;
        if !canonical_path.starts_with(&self.root) {
            bail!("worktree path is outside the configured root");
        }
        let git_root = run_git(
            &canonical_path,
            [
                OsString::from("rev-parse"),
                OsString::from("--show-toplevel"),
            ],
        )?;
        let actual_root = PathBuf::from(git_root).canonicalize()?;
        if actual_root != canonical_path {
            bail!("worktree Git root does not match its registered path");
        }

        let repository_common = git_common_dir(repository_root)?;
        let worktree_common = git_common_dir(&canonical_path)?;
        if repository_common != worktree_common {
            bail!("worktree belongs to a different Git repository");
        }
        let branch = run_git(
            &canonical_path,
            [
                OsString::from("rev-parse"),
                OsString::from("--abbrev-ref"),
                OsString::from("HEAD"),
            ],
        )?;
        if branch != worktree.branch_name {
            bail!("worktree branch does not match its registry record");
        }
        Ok(())
    }

    pub fn integration_worktree(&self, run_id: RunId) -> Result<Worktree> {
        self.find_worktree_by_run_role(run_id, "integration")?
            .ok_or_else(|| anyhow!("run {run_id} has no integration worktree"))
    }

    fn find_agent_worktree(&self, run_id: RunId, agent_id: AgentId) -> Result<Worktree> {
        self.find_worktree_by_run_role(run_id, &WorktreeRole::Agent(agent_id).key())?
            .ok_or_else(|| anyhow!("agent {agent_id} has no worktree for run {run_id}"))
    }

    pub fn get_worktree(&self, id: WorktreeId) -> Result<Worktree> {
        self.ledger.with_connection(|connection| {
            connection
                .query_row(
                    "SELECT id, repo_id, run_id, role_kind, agent_id, base_sha, branch_name,
                            path, state, created_at_ms
                     FROM worktrees WHERE id = ?1",
                    [id.to_string()],
                    worktree_from_row,
                )
                .optional()?
                .ok_or_else(|| anyhow!("worktree {id} is not registered"))
        })
    }

    fn find_worktree_by_run_role(&self, run_id: RunId, role_key: &str) -> Result<Option<Worktree>> {
        self.ledger.with_connection(|connection| {
            connection
                .query_row(
                    "SELECT id, repo_id, run_id, role_kind, agent_id, base_sha, branch_name,
                            path, state, created_at_ms
                     FROM worktrees WHERE run_id = ?1 AND role_key = ?2",
                    params![run_id.to_string(), role_key],
                    worktree_from_row,
                )
                .optional()
                .context("looking up run worktree")
        })
    }

    pub fn list_worktrees(&self) -> Result<Vec<Worktree>> {
        self.ledger.with_connection(|connection| {
            let mut statement = connection.prepare(
                "SELECT id, repo_id, run_id, role_kind, agent_id, base_sha, branch_name,
                        path, state, created_at_ms
                 FROM worktrees ORDER BY created_at_ms, id",
            )?;
            let rows = statement.query_map([], worktree_from_row)?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .context("reading registered worktrees")
        })
    }

    fn insert_worktree(
        &self,
        worktree: &Worktree,
        role_kind: &str,
        agent_id: Option<AgentId>,
    ) -> Result<()> {
        self.ledger.with_connection(|connection| {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            transaction.execute(
                "INSERT INTO worktrees
                    (id, repo_id, run_id, role_kind, role_key, agent_id, base_sha,
                     branch_name, path, state, created_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    worktree.id.to_string(),
                    worktree.repo_id.to_string(),
                    worktree.run_id.to_string(),
                    role_kind,
                    worktree.role.key(),
                    agent_id.map(|id| id.to_string()),
                    worktree.base_sha,
                    worktree.branch_name,
                    path_to_text(&worktree.path)?,
                    state_to_text(worktree.state),
                    worktree.created_at_ms,
                ],
            )?;
            transaction.commit()?;
            Ok(())
        })
    }

    fn update_worktree(&self, worktree: &Worktree) -> Result<()> {
        self.ledger.with_connection(|connection| {
            let changed = connection.execute(
                "UPDATE worktrees SET path = ?2, state = ?3 WHERE id = ?1",
                params![
                    worktree.id.to_string(),
                    path_to_text(&worktree.path)?,
                    state_to_text(worktree.state),
                ],
            )?;
            if changed != 1 {
                bail!(
                    "worktree {} disappeared while updating its record",
                    worktree.id
                );
            }
            Ok(())
        })
    }

    fn require_recovery(&self, id: WorktreeId, kind: &str, detail: &str) -> Result<()> {
        self.ledger.with_connection(|connection| {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            transaction.execute(
                "UPDATE worktrees SET state = 'recovery_required' WHERE id = ?1",
                [id.to_string()],
            )?;
            insert_issue(&transaction, id, kind, detail)?;
            transaction.commit()?;
            Ok(())
        })
    }

    fn record_issue(&self, id: WorktreeId, kind: &str, detail: &str) -> Result<()> {
        self.ledger.with_connection(|connection| {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            insert_issue(&transaction, id, kind, detail)?;
            transaction.commit()?;
            Ok(())
        })
    }

    fn resolve_issues(&self, id: WorktreeId) -> Result<()> {
        self.ledger.with_connection(|connection| {
            connection.execute(
                "UPDATE recovery_issues SET resolved_at_ms = ?2
                 WHERE worktree_id = ?1 AND resolved_at_ms IS NULL",
                params![id.to_string(), now_ms()],
            )?;
            Ok(())
        })
    }
}

fn worktree_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Worktree> {
    let id = parse_uuid(row.get::<_, String>(0)?, 0)?;
    let repo_id = parse_uuid(row.get::<_, String>(1)?, 1)?;
    let run_id = parse_uuid(row.get::<_, String>(2)?, 2)?;
    let role_kind: String = row.get(3)?;
    let agent_id: Option<String> = row.get(4)?;
    let role = match (role_kind.as_str(), agent_id) {
        ("integration", None) => WorktreeRole::Integration,
        ("agent", Some(agent_id)) => WorktreeRole::Agent(AgentId(parse_uuid(agent_id, 4)?)),
        _ => {
            return Err(rusqlite::Error::InvalidColumnType(
                3,
                "role_kind/agent_id".to_owned(),
                rusqlite::types::Type::Text,
            ))
        }
    };
    let state_text: String = row.get(8)?;
    let state = parse_state(&state_text).ok_or_else(|| {
        rusqlite::Error::InvalidColumnType(8, "state".to_owned(), rusqlite::types::Type::Text)
    })?;
    Ok(Worktree {
        id: WorktreeId(id),
        repo_id: RepoId(repo_id),
        run_id: RunId(run_id),
        role,
        base_sha: row.get(5)?,
        branch_name: row.get(6)?,
        path: PathBuf::from(row.get::<_, String>(7)?),
        state,
        created_at_ms: row.get(9)?,
    })
}

fn recovery_issue_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RecoveryIssue> {
    Ok(RecoveryIssue {
        id: parse_uuid(row.get::<_, String>(0)?, 0)?,
        worktree_id: WorktreeId(parse_uuid(row.get::<_, String>(1)?, 1)?),
        kind: row.get(2)?,
        detail: row.get(3)?,
        recorded_at_ms: row.get(4)?,
        resolved_at_ms: row.get(5)?,
    })
}

fn parse_uuid(value: String, index: usize) -> rusqlite::Result<uuid::Uuid> {
    uuid::Uuid::parse_str(&value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })
}

fn state_to_text(state: WorktreeState) -> &'static str {
    match state {
        WorktreeState::Creating => "creating",
        WorktreeState::Active => "active",
        WorktreeState::Archived => "archived",
        WorktreeState::RecoveryRequired => "recovery_required",
    }
}

fn parse_state(state: &str) -> Option<WorktreeState> {
    match state {
        "creating" => Some(WorktreeState::Creating),
        "active" => Some(WorktreeState::Active),
        "archived" => Some(WorktreeState::Archived),
        "recovery_required" => Some(WorktreeState::RecoveryRequired),
        _ => None,
    }
}

fn insert_issue(
    transaction: &rusqlite::Transaction<'_>,
    worktree_id: WorktreeId,
    kind: &str,
    detail: &str,
) -> Result<()> {
    let exists: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM recovery_issues
          WHERE worktree_id = ?1 AND kind = ?2 AND resolved_at_ms IS NULL)",
        params![worktree_id.to_string(), kind],
        |row| row.get(0),
    )?;
    if !exists {
        transaction.execute(
            "INSERT INTO recovery_issues(id, worktree_id, kind, detail, recorded_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                uuid::Uuid::new_v4().to_string(),
                worktree_id.to_string(),
                kind,
                detail,
                now_ms(),
            ],
        )?;
    }
    Ok(())
}

fn resolve_full_commit(repository_root: &Path, sha: &str) -> Result<String> {
    if sha.len() != 40 && sha.len() != 64 || !sha.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("base SHA must be a full hexadecimal Git commit ID");
    }
    let expression = format!("{sha}^{{commit}}");
    let resolved = run_git(
        repository_root,
        [
            OsString::from("rev-parse"),
            OsString::from("--verify"),
            OsString::from(expression),
        ],
    )?;
    if resolved.len() != sha.len() || !resolved.eq_ignore_ascii_case(sha) {
        bail!("base SHA does not identify the exact commit supplied");
    }
    Ok(resolved)
}

fn git_common_dir(root: &Path) -> Result<PathBuf> {
    let common_dir = run_git(
        root,
        [
            OsString::from("rev-parse"),
            OsString::from("--git-common-dir"),
        ],
    )?;
    let path = PathBuf::from(common_dir);
    let path = if path.is_absolute() {
        path
    } else {
        root.join(path)
    };
    path.canonicalize()
        .with_context(|| format!("canonicalizing Git common directory at {}", path.display()))
}

pub(crate) fn normalize_scope_path(path: &str) -> Result<String> {
    if path.is_empty()
        || path.contains('\0')
        || path.contains('\\')
        || Path::new(path).is_absolute()
    {
        bail!("file scope must be a non-empty relative path using '/' separators");
    }
    let mut segments = Vec::new();
    for component in Path::new(path).components() {
        match component {
            Component::Normal(segment) => {
                let segment = segment
                    .to_str()
                    .ok_or_else(|| anyhow!("file scope path is not valid UTF-8"))?;
                segments.push(segment);
            }
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                bail!("file scope cannot escape the repository root");
            }
        }
    }
    if segments.is_empty() {
        bail!("file scope must include at least one path segment");
    }
    Ok(segments.join("/"))
}

fn scopes_overlap(first: &str, second: &str) -> bool {
    first == second
        || first
            .strip_prefix(second)
            .is_some_and(|suffix| suffix.starts_with('/'))
        || second
            .strip_prefix(first)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

fn path_has_symlink(root: &Path, path: &Path) -> Result<bool> {
    if root.canonicalize()? != root {
        return Ok(true);
    }
    if fs::symlink_metadata(root)?.file_type().is_symlink() {
        return Ok(true);
    }
    let relative = path
        .strip_prefix(root)
        .map_err(|_| anyhow!("managed worktree path is outside the configured root"))?;
    let mut current = root.to_path_buf();
    let components = relative.components().collect::<Vec<_>>();
    for (index, component) in components.iter().enumerate() {
        let Component::Normal(part) = component else {
            bail!("managed worktree path contains an invalid component");
        };
        current.push(part);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => return Ok(true),
            Ok(metadata) if index + 1 < components.len() && !metadata.is_dir() => {
                bail!("managed worktree parent is not a directory")
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error.into()),
        }
    }
    Ok(false)
}

fn run_git<I>(cwd: &Path, args: I) -> Result<String>
where
    I: IntoIterator<Item = OsString>,
{
    let mut command = Command::new("git");
    command.arg("-C").arg(cwd).args(args);
    let output = command.output().context("starting git command")?;
    git_stdout(output)
}

fn git_stdout(output: Output) -> Result<String> {
    if !output.status.success() {
        bail!("git command failed with status {}", output.status);
    }
    String::from_utf8(output.stdout)
        .map(|output| output.trim().to_owned())
        .context("Git output is not valid UTF-8")
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::repositories::RepositoryRegistry;
    use std::{
        fs,
        fs::OpenOptions,
        os::{fd::AsRawFd, unix::fs::OpenOptionsExt},
    };
    use tempfile::tempdir;

    #[test]
    fn startup_reconciliation_skips_a_worktree_held_by_a_live_creator() -> Result<()> {
        let temp = tempdir()?;
        let database_path = temp.path().join("factory.sqlite");
        let repository_root = temp.path().join("repository");
        fs::create_dir_all(&repository_root)?;
        let init = Command::new("git")
            .args(["init", "--initial-branch=main"])
            .current_dir(&repository_root)
            .status()?;
        anyhow::ensure!(init.success(), "could not initialize test Git repository");
        let ledger = Ledger::open(&database_path)?;
        let repository = RepositoryRegistry::new(ledger.clone()).register(&repository_root)?;
        let manager = WorktreeManager::new(ledger, temp.path().join("worktrees"))?;
        let worktree = Worktree {
            id: WorktreeId::new(),
            repo_id: repository.id,
            run_id: RunId::new(),
            role: WorktreeRole::Integration,
            base_sha: "0".repeat(40),
            branch_name: "factory/run-live-creation".to_owned(),
            path: temp.path().join("worktrees").join("pending-worktree"),
            state: WorktreeState::Creating,
            created_at_ms: now_ms(),
        };
        manager.insert_worktree(&worktree, "integration", None)?;

        let locks = temp.path().join("worktrees").join(".locks");
        fs::create_dir_all(&locks)?;
        let lock_file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(locks.join(format!("{}.lock", worktree.id)))?;
        let acquired = unsafe { libc::flock(lock_file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        anyhow::ensure!(
            acquired == 0,
            "could not acquire simulated live-creator lock"
        );

        let second_manager =
            WorktreeManager::new(Ledger::open(&database_path)?, temp.path().join("worktrees"))?;
        assert!(second_manager.reconcile()?.is_empty());
        assert_eq!(
            second_manager.get_worktree(worktree.id)?.state,
            WorktreeState::Creating
        );
        Ok(())
    }
}
