use crate::{
    ledger::Ledger,
    model::{RepoId, Repository},
};
use anyhow::{anyhow, bail, Context, Result};
use rusqlite::{params, OptionalExtension, TransactionBehavior};
use std::{
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone)]
pub struct RepositoryRegistry {
    ledger: Ledger,
}

impl RepositoryRegistry {
    pub fn new(ledger: Ledger) -> Self {
        Self { ledger }
    }

    pub fn register(&self, path: &Path) -> Result<Repository> {
        let root = git_root(path)?;
        let root = root
            .canonicalize()
            .with_context(|| format!("canonicalizing Git repository root at {}", root.display()))?;
        let root_text = path_to_text(&root)?.to_owned();
        let remote_url = discover_remote(&root)?;
        let default_branch = discover_default_branch(&root)?;
        let registered_at_ms = now_ms();

        self.ledger.with_connection(|connection| {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            if let Some(repository) = transaction
                .query_row(
                    "SELECT id, canonical_root, remote_url, default_branch, registered_at_ms
                     FROM repositories WHERE canonical_root = ?1",
                    [&root_text],
                    repository_from_row,
                )
                .optional()?
            {
                transaction.commit()?;
                return Ok(repository);
            }

            let repository = Repository {
                id: RepoId::new(),
                canonical_root: root,
                remote_url,
                default_branch,
                registered_at_ms,
            };
            transaction.execute(
                "INSERT INTO repositories
                    (id, canonical_root, remote_url, default_branch, registered_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    repository.id.to_string(),
                    root_text,
                    repository.remote_url,
                    repository.default_branch,
                    repository.registered_at_ms,
                ],
            )?;
            transaction.commit()?;
            Ok(repository)
        })
    }

    pub fn get(&self, id: RepoId) -> Result<Repository> {
        self.ledger.with_connection(|connection| {
            connection
                .query_row(
                    "SELECT id, canonical_root, remote_url, default_branch, registered_at_ms
                     FROM repositories WHERE id = ?1",
                    [id.to_string()],
                    repository_from_row,
                )
                .optional()?
                .ok_or_else(|| anyhow!("repository {id} is not registered"))
        })
    }

    pub fn list(&self) -> Result<Vec<Repository>> {
        self.ledger.with_connection(|connection| {
            let mut statement = connection.prepare(
                "SELECT id, canonical_root, remote_url, default_branch, registered_at_ms
                 FROM repositories ORDER BY canonical_root COLLATE NOCASE",
            )?;
            let rows = statement.query_map([], repository_from_row)?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .context("reading registered repositories")
        })
    }
}

fn repository_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Repository> {
    let id = row.get::<_, String>(0)?;
    let canonical_root = row.get::<_, String>(1)?;
    Ok(Repository {
        id: RepoId(uuid::Uuid::parse_str(&id).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?),
        canonical_root: PathBuf::from(canonical_root),
        remote_url: row.get(2)?,
        default_branch: row.get(3)?,
        registered_at_ms: row.get(4)?,
    })
}

fn git_root(path: &Path) -> Result<PathBuf> {
    if !path.is_dir() {
        bail!("repository path is not a directory: {}", path.display());
    }
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .context("starting git to inspect the repository root")?;
    if !output.status.success() {
        bail!("selected directory is not inside a Git working tree");
    }
    let text = String::from_utf8(output.stdout).context("Git root path is not UTF-8")?;
    Ok(PathBuf::from(text.trim()))
}

pub(crate) fn path_to_text(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| anyhow!("repository and worktree paths must be valid UTF-8"))
}

fn discover_remote(root: &Path) -> Result<Option<String>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["remote"])
        .output()
        .context("reading Git remotes")?;
    if !output.status.success() {
        bail!("could not inspect Git remotes");
    }
    let remotes = String::from_utf8(output.stdout).context("Git remote name is not UTF-8")?;
    let mut names = remotes.lines().filter(|name| !name.starts_with('-'));
    let remote_name = names
        .clone()
        .find(|name| *name == "origin")
        .or_else(|| names.next());
    let Some(remote_name) = remote_name else {
        return Ok(None);
    };

    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["remote", "get-url", remote_name])
        .output()
        .context("reading the repository remote URL")?;
    if !output.status.success() {
        return Ok(None);
    }
    let raw = String::from_utf8(output.stdout).context("Git remote URL is not UTF-8")?;
    Ok(sanitize_remote(raw.trim()))
}

fn sanitize_remote(raw: &str) -> Option<String> {
    if raw.is_empty() {
        return None;
    }
    let without_query = raw.split(['?', '#']).next().unwrap_or_default();
    if let Some((scheme, rest)) = without_query.split_once("://") {
        let authority_end = rest.find('/').unwrap_or(rest.len());
        let (authority, suffix) = rest.split_at(authority_end);
        let host = authority.rsplit('@').next().unwrap_or_default();
        if host.is_empty() {
            return None;
        }
        return Some(format!("{scheme}://{host}{suffix}"));
    }

    if let Some((user, host_and_path)) = without_query.split_once('@') {
        if host_and_path.contains(':') && !host_and_path.starts_with('/') {
            let safe_user = if user == "git" { "git@" } else { "" };
            return Some(format!("{safe_user}{host_and_path}"));
        }
    }
    Some(without_query.to_owned())
}

fn discover_default_branch(root: &Path) -> Result<String> {
    let remote_heads = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["remote"])
        .output()
        .context("reading Git remotes")?;
    if !remote_heads.status.success() {
        bail!("could not inspect Git remotes");
    }
    let remotes = String::from_utf8(remote_heads.stdout).context("Git remote name is not UTF-8")?;
    let remote_names = remotes.lines().filter(|name| !name.starts_with('-'));
    for remote in remote_names {
        let symbolic_ref = format!("refs/remotes/{remote}/HEAD");
        let output = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["symbolic-ref", "--quiet", "--short", &symbolic_ref])
            .output()
            .context("reading the remote default branch")?;
        if output.status.success() {
            let reference = String::from_utf8(output.stdout)
                .context("remote default branch name is not UTF-8")?;
            let prefix = format!("{remote}/");
            if let Some(branch) = reference.trim().strip_prefix(&prefix) {
                if !branch.is_empty() {
                    return Ok(branch.to_owned());
                }
            }
        }
    }

    let branches = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["for-each-ref", "--format=%(refname:short)", "refs/heads"])
        .output()
        .context("reading local branch names")?;
    if !branches.status.success() {
        bail!("could not inspect local Git branches");
    }
    let branch_text = String::from_utf8(branches.stdout).context("Git branch name is not UTF-8")?;
    let branch_names = branch_text.lines().collect::<Vec<_>>();
    for preferred in ["main", "master"] {
        if branch_names.contains(&preferred) {
            return Ok(preferred.to_owned());
        }
    }
    let head = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["symbolic-ref", "--quiet", "--short", "HEAD"])
        .output()
        .context("reading the current branch for an unborn repository")?;
    if head.status.success() {
        let branch =
            String::from_utf8(head.stdout).context("current Git branch name is not UTF-8")?;
        if !branch.trim().is_empty() {
            return Ok(branch.trim().to_owned());
        }
    }
    branch_names
        .first()
        .map(|branch| (*branch).to_owned())
        .ok_or_else(|| anyhow!("repository has no branch to use as its default"))
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}
