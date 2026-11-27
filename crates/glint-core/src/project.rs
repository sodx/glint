//! Which project an event belongs to, when the client did not say.
//!
//! The project is the name of the git repository the code lives in, so that every linked
//! worktree of a repository (`git worktree add`, Claude Code's `.claude/worktrees/<name>`,
//! Conductor, …) lands in the same project instead of one project per folder. The worktree, if
//! any, is kept on the event as `origin.worktree` (and its branch as `origin.branch`) so the UI
//! can still tell them apart.
//!
//! Resolution, from `origin.file`'s directory, else `origin.cwd`:
//! 1. walk up to the nearest `.git`;
//!    - a `.git` directory: the repository is that directory's parent;
//!    - a `.git` file (`gitdir: <repo>/.git/worktrees/<name>`): a linked worktree; the repository
//!      is the one owning the common git dir (bare `repo.git` included);
//!    - a `.git` file without `/worktrees/` (a submodule): the submodule's own folder.
//! 2. no `.git` on this machine (a container path, a deleted folder): well-known worktree
//!    layouts in the path itself (`…/<repo>/.claude/worktrees/<name>`, `…/<repo>/.worktrees/<name>`);
//! 3. else (in the normaliser) the name of the process's working directory.
//!
//! Lookups touch the filesystem, so results are cached per directory for a minute.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolved {
    pub project: String,
    /// The linked worktree's folder name, when the code runs in one.
    pub worktree: Option<String>,
    /// The worktree's checked-out branch, when it could be read.
    pub branch: Option<String>,
}

const TTL: Duration = Duration::from_secs(60);
const MAX_ENTRIES: usize = 4096;

#[derive(Default)]
pub struct ProjectResolver {
    cache: HashMap<String, (Instant, Option<Resolved>)>,
}

impl ProjectResolver {
    pub fn resolve(&mut self, dir: &str) -> Option<Resolved> {
        let now = Instant::now();
        if let Some((at, r)) = self.cache.get(dir) {
            if now.duration_since(*at) < TTL {
                return r.clone();
            }
        }
        if self.cache.len() >= MAX_ENTRIES {
            self.cache.retain(|_, (at, _)| now.duration_since(*at) < TTL);
            if self.cache.len() >= MAX_ENTRIES {
                self.cache.clear();
            }
        }
        let r = resolve(dir);
        self.cache.insert(dir.to_owned(), (now, r.clone()));
        r
    }
}

/// Uncached resolution (steps 1–2 of the module docs); `None` when neither finds anything.
pub fn resolve(dir: &str) -> Option<Resolved> {
    let trimmed = dir.trim_end_matches(['/', '\\']);
    if trimmed.is_empty() {
        return None;
    }
    let path = Path::new(trimmed);
    if path.is_absolute() {
        for d in path.ancestors() {
            let git = d.join(".git");
            if git.is_dir() {
                return Some(Resolved { project: name(d)?, worktree: None, branch: None });
            }
            if git.is_file() {
                return Some(from_git_file(d, &git).unwrap_or(Resolved { project: name(d)?, worktree: None, branch: None }));
            }
        }
    }
    from_layout(trimmed)
}

/// The last component of a path: the project when nothing better is known.
pub fn folder_name(dir: &str) -> Option<String> {
    dir.trim_end_matches(['/', '\\']).rsplit(['/', '\\']).next().filter(|s| !s.is_empty()).map(str::to_owned)
}

/// `<worktree>/.git` is a file: `gitdir: /src/shop/.git/worktrees/feat-x`.
fn from_git_file(worktree: &Path, git: &Path) -> Option<Resolved> {
    let text = std::fs::read_to_string(git).ok()?;
    let gitdir = text.lines().find_map(|l| l.strip_prefix("gitdir:"))?.trim();
    let mut gitdir = PathBuf::from(gitdir);
    if gitdir.is_relative() {
        gitdir = worktree.join(gitdir);
    }
    let parent = gitdir.parent()?;
    if parent.file_name()? != "worktrees" {
        return None; // a submodule: its own folder is the project
    }
    let common = parent.parent()?; // /src/shop/.git, or /src/shop.git when bare
    let project = if common.file_name()? == ".git" {
        name(common.parent()?)?
    } else {
        let n = name(common)?;
        n.strip_suffix(".git").filter(|s| !s.is_empty()).map(str::to_owned).unwrap_or(n)
    };
    let branch = std::fs::read_to_string(gitdir.join("HEAD"))
        .ok()
        .and_then(|h| h.trim().strip_prefix("ref: refs/heads/").map(str::to_owned));
    Some(Resolved { project, worktree: name(worktree), branch })
}

/// Worktree layouts recognisable from the path alone: `<repo>/.claude/worktrees/<name>` and
/// `<repo>/.worktrees/<name>`.
#[allow(clippy::needless_range_loop)]
fn from_layout(path: &str) -> Option<Resolved> {
    let parts: Vec<&str> = path.split(['/', '\\']).collect();
    let at = |i: usize| parts.get(i).copied().filter(|s| !s.is_empty());
    for i in 1..parts.len() {
        let wt = match parts[i] {
            ".claude" if at(i + 1) == Some("worktrees") => at(i + 2),
            ".worktrees" => at(i + 1),
            _ => None,
        };
        if let (Some(repo), Some(wt)) = (at(i - 1), wt) {
            return Some(Resolved { project: repo.to_owned(), worktree: Some(wt.to_owned()), branch: None });
        }
    }
    None
}

fn name(p: &Path) -> Option<String> {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn sandbox(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("glint-project-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d.canonicalize().unwrap()
    }

    fn r(project: &str, worktree: Option<&str>, branch: Option<&str>) -> Option<Resolved> {
        Some(Resolved { project: project.into(), worktree: worktree.map(Into::into), branch: branch.map(Into::into) })
    }

    #[test]
    fn main_checkout_and_subfolders_are_the_repository() {
        let s = sandbox("main");
        fs::create_dir_all(s.join("shop/.git")).unwrap();
        fs::create_dir_all(s.join("shop/src/cart")).unwrap();
        assert_eq!(resolve(&s.join("shop/src/cart").to_string_lossy()), r("shop", None, None));
        assert_eq!(resolve(&format!("{}/", s.join("shop").display())), r("shop", None, None));
        fs::remove_dir_all(&s).unwrap();
    }

    #[test]
    fn linked_worktrees_belong_to_their_repository() {
        let s = sandbox("wt");
        let admin = s.join("shop/.git/worktrees/epic-ishizaka");
        fs::create_dir_all(&admin).unwrap();
        fs::write(admin.join("HEAD"), "ref: refs/heads/feat/cart-total\n").unwrap();
        let wt = s.join("shop/.claude/worktrees/epic-ishizaka");
        fs::create_dir_all(wt.join("src")).unwrap();
        fs::write(wt.join(".git"), format!("gitdir: {}\n", admin.display())).unwrap();
        assert_eq!(resolve(&wt.join("src").to_string_lossy()), r("shop", Some("epic-ishizaka"), Some("feat/cart-total")));

        // A worktree elsewhere on disk, and a bare repository, still name the repository.
        let bare = s.join("api.git/worktrees/hotfix");
        fs::create_dir_all(&bare).unwrap();
        fs::write(bare.join("HEAD"), "0123abcd\n").unwrap(); // detached
        let far = s.join("elsewhere/hotfix");
        fs::create_dir_all(&far).unwrap();
        fs::write(far.join(".git"), format!("gitdir: {}", bare.display())).unwrap();
        assert_eq!(resolve(&far.to_string_lossy()), r("api", Some("hotfix"), None));
        fs::remove_dir_all(&s).unwrap();
    }

    #[test]
    fn submodules_are_their_own_project() {
        let s = sandbox("sub");
        fs::create_dir_all(s.join("app/.git/modules/ui")).unwrap();
        fs::create_dir_all(s.join("app/ui")).unwrap();
        fs::write(s.join("app/ui/.git"), "gitdir: ../.git/modules/ui\n").unwrap();
        assert_eq!(resolve(&s.join("app/ui").to_string_lossy()), r("ui", None, None));
        fs::remove_dir_all(&s).unwrap();
    }

    #[test]
    fn paths_from_other_machines_use_known_layouts() {
        assert_eq!(resolve("/workspace/shop/.claude/worktrees/epic-ishizaka/src"), r("shop", Some("epic-ishizaka"), None));
        assert_eq!(resolve("C:\\src\\shop\\.worktrees\\feat-x"), r("shop", Some("feat-x"), None));
        assert_eq!(resolve("/no/such/place/payments-service/"), None);
        assert_eq!(folder_name("/no/such/place/payments-service/").as_deref(), Some("payments-service"));
        assert_eq!(resolve("/"), None);
    }

    #[test]
    fn real_git_worktree() {
        let s = sandbox("git");
        let git = |args: &[&str], dir: &Path| std::process::Command::new("git").args(args).current_dir(dir).output();
        let repo = s.join("shop");
        fs::create_dir_all(&repo).unwrap();
        if !git(&["init", "-q", "-b", "main"], &repo).is_ok_and(|o| o.status.success()) {
            return; // no git here
        }
        let ok = git(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "x"], &repo)
            .is_ok_and(|o| o.status.success())
            && git(&["worktree", "add", "-q", "-b", "feat-x", "../shop-feat-x"], &repo).is_ok_and(|o| o.status.success());
        assert!(ok, "git worktree add failed");
        assert_eq!(resolve(&s.join("shop-feat-x").to_string_lossy()), r("shop", Some("shop-feat-x"), Some("feat-x")));
        fs::remove_dir_all(&s).unwrap();
    }
}
