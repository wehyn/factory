use factory_core::{
    AgentId, ArchiveOutcome, Ledger, RepositoryRegistry, RunId, WorktreeManager, WorktreeStatus,
};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};
use tempfile::TempDir;
use uuid::Uuid;

struct GitRepository {
    _temp: TempDir,
    root: PathBuf,
    base_sha: String,
}

fn init_repository() -> anyhow::Result<GitRepository> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("repo");
    fs::create_dir_all(&root)?;
    git(&root, &["init", "--initial-branch=main"])?;
    git(&root, &["config", "user.name", "Factory Test"])?;
    git(
        &root,
        &["config", "user.email", "factory-test@example.invalid"],
    )?;
    fs::write(root.join("README.md"), "base\n")?;
    fs::write(root.join(".gitignore"), "ignored-output/\n")?;
    git(&root, &["add", "README.md", ".gitignore"])?;
    git(&root, &["commit", "-m", "initial"])?;
    let base_sha = git(&root, &["rev-parse", "HEAD"])?;

    Ok(GitRepository {
        _temp: temp,
        root,
        base_sha,
    })
}

fn git(root: &Path, args: &[&str]) -> anyhow::Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()?;
    if !output.status.success() {
        anyhow::bail!(
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn services(temp: &TempDir) -> anyhow::Result<(Ledger, RepositoryRegistry, WorktreeManager)> {
    let ledger = Ledger::open(&temp.path().join("factory.sqlite"))?;
    let registry = RepositoryRegistry::new(ledger.clone());
    let worktrees = WorktreeManager::new(ledger.clone(), temp.path().join("worktrees"))?;
    Ok((ledger, registry, worktrees))
}

#[test]
fn registers_the_canonical_git_root_once_and_recovers_it_from_sqlite() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let repository = init_repository()?;
    fs::create_dir_all(repository.root.join("nested"))?;
    let (ledger, registry, _) = services(&temp)?;

    let registered = registry.register(&repository.root.join("nested"))?;
    let duplicate = registry.register(&repository.root)?;
    assert_eq!(registered.id, duplicate.id);
    assert_eq!(registered.canonical_root, repository.root.canonicalize()?);
    assert_eq!(registered.default_branch, "main");
    assert_eq!(registry.list()?.len(), 1);

    drop(registry);
    drop(ledger);
    let reopened = Ledger::open(&temp.path().join("factory.sqlite"))?;
    let restored = RepositoryRegistry::new(reopened).list()?;
    assert_eq!(restored.len(), 1);
    assert_eq!(restored[0].canonical_root, repository.root.canonicalize()?);
    assert_eq!(restored[0].default_branch, "main");
    Ok(())
}

#[test]
fn repository_credentials_are_not_retained_in_registry_metadata() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let repository = init_repository()?;
    git(
        &repository.root,
        &[
            "remote",
            "add",
            "origin",
            "https://alice:remote-secret@github.com/owner/repo.git?access_token=query-secret",
        ],
    )?;
    let (ledger, registry, _) = services(&temp)?;

    let registered = registry.register(&repository.root)?;
    assert_eq!(
        registered.remote_url.as_deref(),
        Some("https://github.com/owner/repo.git")
    );
    drop(registry);
    drop(ledger);
    let mut database = fs::read(temp.path().join("factory.sqlite"))?;
    if let Ok(wal) = fs::read(temp.path().join("factory.sqlite-wal")) {
        database.extend(wal);
    }
    assert!(!database
        .windows(b"remote-secret".len())
        .any(|window| window == b"remote-secret"));
    assert!(!database
        .windows(b"query-secret".len())
        .any(|window| window == b"query-secret"));
    Ok(())
}

#[test]
fn agent_worktrees_start_at_the_recorded_base_and_isolate_edits() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let repository = init_repository()?;
    let (_ledger, registry, worktrees) = services(&temp)?;
    let registered = registry.register(&repository.root)?;
    let run_id = RunId(Uuid::new_v4());
    let integration = worktrees.create_run_worktree(registered.id, run_id, &repository.base_sha)?;
    let agent_a =
        worktrees.create_agent_worktree(run_id, AgentId(Uuid::new_v4()), &repository.base_sha)?;
    let agent_b =
        worktrees.create_agent_worktree(run_id, AgentId(Uuid::new_v4()), &repository.base_sha)?;

    let listed_paths = git(&repository.root, &["worktree", "list", "--porcelain"])?
        .lines()
        .filter_map(|line| line.strip_prefix("worktree "))
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    for worktree in [&integration, &agent_a, &agent_b] {
        assert!(listed_paths.contains(&worktree.path.canonicalize()?));
    }

    assert_eq!(
        git(&agent_a.path, &["rev-parse", "HEAD"])?,
        repository.base_sha
    );
    assert_eq!(
        git(&agent_b.path, &["rev-parse", "HEAD"])?,
        repository.base_sha
    );
    fs::write(agent_a.path.join("README.md"), "agent a\n")?;
    fs::write(agent_b.path.join("README.md"), "agent b\n")?;

    assert_eq!(
        fs::read_to_string(agent_a.path.join("README.md"))?,
        "agent a\n"
    );
    assert_eq!(
        fs::read_to_string(agent_b.path.join("README.md"))?,
        "agent b\n"
    );
    assert_eq!(
        fs::read_to_string(integration.path.join("README.md"))?,
        "base\n"
    );
    assert_eq!(
        worktrees.inspect_worktree(agent_a.id)?,
        WorktreeStatus::Dirty
    );
    Ok(())
}

#[test]
fn rejects_non_sha_base_values_before_creating_a_git_worktree() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let repository = init_repository()?;
    let (_ledger, registry, worktrees) = services(&temp)?;
    let registered = registry.register(&repository.root)?;

    assert!(worktrees
        .create_run_worktree(registered.id, RunId(Uuid::new_v4()), "HEAD")
        .is_err());
    assert_eq!(
        git(&repository.root, &["worktree", "list", "--porcelain"])?
            .matches("worktree ")
            .count(),
        1
    );
    Ok(())
}

#[test]
fn archive_refuses_dirty_worktrees_and_preserves_untracked_data() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let repository = init_repository()?;
    let (_ledger, registry, worktrees) = services(&temp)?;
    let registered = registry.register(&repository.root)?;
    let run_id = RunId(Uuid::new_v4());
    let worktree = worktrees.create_run_worktree(registered.id, run_id, &repository.base_sha)?;
    let untracked = worktree.path.join("notes.txt");
    let ignored = worktree.path.join("ignored-output/kept.log");
    fs::write(&untracked, "work that must survive\n")?;
    fs::create_dir_all(ignored.parent().expect("ignored file has a parent"))?;
    fs::write(&ignored, "ignored data must survive\n")?;

    assert_eq!(
        worktrees.inspect_worktree(worktree.id)?,
        WorktreeStatus::Dirty
    );
    assert_eq!(
        worktrees.archive_worktree(worktree.id)?,
        ArchiveOutcome::RefusedDirty
    );
    assert!(worktree.path.is_dir());
    assert_eq!(fs::read_to_string(untracked)?, "work that must survive\n");
    assert_eq!(fs::read_to_string(ignored)?, "ignored data must survive\n");
    assert!(git(&repository.root, &["worktree", "list", "--porcelain"])?
        .contains(worktree.path.to_string_lossy().as_ref()));
    Ok(())
}

#[test]
fn archive_moves_a_clean_worktree_and_preserves_its_branch_and_files() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let repository = init_repository()?;
    let (_ledger, registry, worktrees) = services(&temp)?;
    let registered = registry.register(&repository.root)?;
    let run_id = RunId(Uuid::new_v4());
    let worktree = worktrees.create_run_worktree(registered.id, run_id, &repository.base_sha)?;
    let archived_path = temp
        .path()
        .join("worktrees")
        .join("archive")
        .join(worktree.id.to_string());

    assert_eq!(
        worktrees.archive_worktree(worktree.id)?,
        ArchiveOutcome::Archived
    );
    assert!(!worktree.path.exists());
    assert_eq!(
        fs::read_to_string(archived_path.join("README.md"))?,
        "base\n"
    );
    assert!(git(
        &repository.root,
        &["branch", "--list", &worktree.branch_name]
    )?
    .contains(&worktree.branch_name));
    assert!(git(&repository.root, &["worktree", "list", "--porcelain"])?
        .contains(archived_path.to_string_lossy().as_ref()));
    assert_eq!(
        worktrees.inspect_worktree(worktree.id)?,
        WorktreeStatus::Archived
    );
    Ok(())
}

#[test]
fn deleted_archived_worktrees_become_persisted_recovery_issues() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let repository = init_repository()?;
    let (_ledger, registry, worktrees) = services(&temp)?;
    let registered = registry.register(&repository.root)?;
    let worktree = worktrees.create_run_worktree(
        registered.id,
        RunId(Uuid::new_v4()),
        &repository.base_sha,
    )?;
    let archived_path = temp
        .path()
        .join("worktrees")
        .join("archive")
        .join(worktree.id.to_string());
    assert_eq!(
        worktrees.archive_worktree(worktree.id)?,
        ArchiveOutcome::Archived
    );
    fs::remove_dir_all(archived_path)?;

    assert_eq!(worktrees.reconcile()?.len(), 1);
    assert_eq!(
        worktrees.inspect_worktree(worktree.id)?,
        WorktreeStatus::RecoveryRequired
    );
    assert_eq!(worktrees.recovery_issues()?.len(), 1);
    Ok(())
}

#[cfg(unix)]
#[test]
fn refuses_a_worktree_path_replaced_by_a_symlink() -> anyhow::Result<()> {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir()?;
    let repository = init_repository()?;
    let (_ledger, registry, worktrees) = services(&temp)?;
    let registered = registry.register(&repository.root)?;
    let worktree = worktrees.create_run_worktree(
        registered.id,
        RunId(Uuid::new_v4()),
        &repository.base_sha,
    )?;
    assert_eq!(
        worktrees.archive_worktree(worktree.id)?,
        ArchiveOutcome::Archived
    );
    let archived_path = temp
        .path()
        .join("worktrees")
        .join("archive")
        .join(worktree.id.to_string());
    let relocated = temp.path().join("relocated-worktree");
    fs::rename(&archived_path, &relocated)?;
    symlink(&relocated, &archived_path)?;

    assert_eq!(
        worktrees.inspect_worktree(worktree.id)?,
        WorktreeStatus::Unsafe
    );
    assert_eq!(
        worktrees.archive_worktree(worktree.id)?,
        ArchiveOutcome::RefusedUnsafe
    );
    assert_eq!(fs::read_to_string(relocated.join("README.md"))?, "base\n");
    assert_eq!(worktrees.recovery_issues()?.len(), 1);
    Ok(())
}

#[test]
fn overlapping_file_scope_claims_conflict_until_the_manager_releases_them() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let repository = init_repository()?;
    let (_ledger, registry, worktrees) = services(&temp)?;
    let registered = registry.register(&repository.root)?;
    let run_id = RunId(Uuid::new_v4());
    let first = AgentId(Uuid::new_v4());
    let second = AgentId(Uuid::new_v4());
    worktrees.create_run_worktree(registered.id, run_id, &repository.base_sha)?;
    worktrees.create_agent_worktree(run_id, first, &repository.base_sha)?;
    worktrees.create_agent_worktree(run_id, second, &repository.base_sha)?;

    worktrees.claim_file_scope(run_id, first, &["src"])?;
    assert!(worktrees
        .claim_file_scope(run_id, second, &["src/App.tsx"])
        .is_err());
    worktrees.release_file_scope(run_id, first)?;
    worktrees.claim_file_scope(run_id, second, &["src/App.tsx"])?;
    assert!(worktrees
        .claim_file_scope(run_id, AgentId(Uuid::new_v4()), &["../secrets"])
        .is_err());
    Ok(())
}

#[test]
fn competing_sqlite_connections_cannot_claim_overlapping_scopes() -> anyhow::Result<()> {
    use std::sync::{Arc, Barrier};

    let temp = tempfile::tempdir()?;
    let repository = init_repository()?;
    let (ledger, registry, first_manager) = services(&temp)?;
    let registered = registry.register(&repository.root)?;
    let run_id = RunId(Uuid::new_v4());
    let agent_a = AgentId(Uuid::new_v4());
    let agent_b = AgentId(Uuid::new_v4());
    first_manager.create_run_worktree(registered.id, run_id, &repository.base_sha)?;
    first_manager.create_agent_worktree(run_id, agent_a, &repository.base_sha)?;
    first_manager.create_agent_worktree(run_id, agent_b, &repository.base_sha)?;
    let second_ledger = Ledger::open(&temp.path().join("factory.sqlite"))?;
    let second_manager = WorktreeManager::new(second_ledger, temp.path().join("worktrees"))?;
    drop(ledger);

    let barrier = Arc::new(Barrier::new(3));
    let claim_a = {
        let manager = first_manager.clone();
        let barrier = Arc::clone(&barrier);
        std::thread::spawn(move || {
            barrier.wait();
            manager.claim_file_scope(run_id, agent_a, &["src"])
        })
    };
    let claim_b = {
        let manager = second_manager;
        let barrier = Arc::clone(&barrier);
        std::thread::spawn(move || {
            barrier.wait();
            manager.claim_file_scope(run_id, agent_b, &["src/App.tsx"])
        })
    };
    barrier.wait();
    let first_won = claim_a.join().expect("first claim thread panicked").is_ok();
    let second_won = claim_b
        .join()
        .expect("second claim thread panicked")
        .is_ok();
    assert_ne!(first_won, second_won);
    Ok(())
}

#[test]
fn missing_worktrees_are_reported_as_persisted_recovery_issues() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let repository = init_repository()?;
    let (_ledger, registry, worktrees) = services(&temp)?;
    let registered = registry.register(&repository.root)?;
    let run_id = RunId(Uuid::new_v4());
    let worktree = worktrees.create_run_worktree(registered.id, run_id, &repository.base_sha)?;
    fs::remove_dir_all(&worktree.path)?;

    assert_eq!(
        worktrees.inspect_worktree(worktree.id)?,
        WorktreeStatus::Missing
    );
    assert_eq!(worktrees.recovery_issues()?.len(), 1);

    drop(worktrees);
    let reopened = Ledger::open(&temp.path().join("factory.sqlite"))?;
    let restored = WorktreeManager::new(reopened, temp.path().join("worktrees"))?;
    assert_eq!(restored.recovery_issues()?.len(), 1);
    Ok(())
}
