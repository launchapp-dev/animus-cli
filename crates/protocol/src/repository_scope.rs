use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

pub fn scoped_state_root(project_root: &Path) -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    let ao_root = home.join(".animus");
    let scope_dir = ao_root.join(repository_scope_for_path(project_root));

    if scope_dir.exists() {
        reclaim_scope_marker_if_foreign(&scope_dir, project_root);
        return Some(scope_dir);
    }

    if let Some(existing) = find_existing_scope_by_origin(&ao_root, project_root) {
        persist_project_root_marker(&existing, project_root);
        return Some(existing);
    }

    if !scope_dir.exists() {
        if std::fs::create_dir_all(&scope_dir).is_err() {
            return Some(scope_dir);
        }
        persist_project_root_marker(&scope_dir, project_root);
        if let Some(origin) = git_remote_origin(project_root) {
            let _ = std::fs::write(scope_dir.join(".git-origin"), origin);
        }
    }

    Some(scope_dir)
}

// The scope dir's NAME is the hash of the CURRENT caller's canonical path, so
// the caller holds the stronger claim on it. If the `.project-root` marker
// points at a different live path (a sibling clone reclaimed this scope while
// our path was unreachable, e.g. its volume was unmounted), reclaim the marker
// back and warn loudly. A marker ping-pong between two live clones is
// detectable in logs; silently sharing workflow.db, runs, and worktrees across
// clones is not.
fn reclaim_scope_marker_if_foreign(scope_dir: &Path, project_root: &Path) {
    let marker_path = scope_dir.join(".project-root");
    let Ok(recorded_raw) = std::fs::read_to_string(&marker_path) else {
        persist_project_root_marker(scope_dir, project_root);
        return;
    };
    let canonical = project_root.canonicalize().unwrap_or_else(|_| project_root.to_path_buf());
    let recorded = Path::new(recorded_raw.trim());
    match recorded.canonicalize() {
        Ok(recorded_canonical) if paths_refer_to_same_file(&recorded_canonical, &canonical) => {}
        Ok(recorded_canonical) => {
            tracing::warn!(
                scope_dir = %scope_dir.display(),
                recorded_root = %recorded_canonical.display(),
                current_root = %canonical.display(),
                "scope marker points at a different live clone; reclaiming the scope for the current project root"
            );
            persist_project_root_marker(scope_dir, project_root);
        }
        Err(_) => {
            persist_project_root_marker(scope_dir, project_root);
        }
    }
}

fn persist_project_root_marker(scope_dir: &Path, project_root: &Path) {
    let canonical = project_root.canonicalize().unwrap_or_else(|_| project_root.to_path_buf());
    let _ = std::fs::write(scope_dir.join(".project-root"), format!("{}\n", canonical.to_string_lossy()));
}

fn git_remote_origin(project_root: &Path) -> Option<String> {
    std::process::Command::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(project_root)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
}

fn find_existing_scope_by_origin(ao_root: &Path, project_root: &Path) -> Option<PathBuf> {
    let our_origin = git_remote_origin(project_root)?;
    let canonical = project_root.canonicalize().unwrap_or_else(|_| project_root.to_path_buf());

    let entries = std::fs::read_dir(ao_root).ok()?;
    for entry in entries.flatten() {
        let scope_dir = entry.path();
        if !scope_dir.is_dir() {
            continue;
        }

        let origin_file = scope_dir.join(".git-origin");
        let Ok(existing_origin) = std::fs::read_to_string(&origin_file) else {
            continue;
        };
        if existing_origin.trim() != our_origin {
            continue;
        }

        // Same remote origin found. To avoid cross-clone collisions (two
        // separate checkouts of the same repo sharing workflow.db, logs, and
        // worktrees), require that the candidate scope's recorded
        // `.project-root` resolves to the same canonical path we are being
        // asked about. If the marker points to a different existing path,
        // that scope belongs to a sibling clone — skip it and let the caller
        // fall through to creating the hash-derived scope.
        //
        // Adopting a same-origin scope is still allowed when:
        //   * no `.project-root` marker exists (legacy/unmigrated scope), or
        //   * the recorded path no longer canonicalizes because the repo was
        //     moved or deleted (the historical scope should remain reachable
        //     from the new path) — but NOT when the recorded path merely sits
        //     on an unmounted volume; see
        //     `recorded_path_is_transiently_unavailable`.
        let project_root_file = scope_dir.join(".project-root");
        match std::fs::read_to_string(&project_root_file) {
            Ok(existing_root) => {
                let recorded = Path::new(existing_root.trim());
                match recorded.canonicalize() {
                    // Same-file comparison (dev+ino on unix) rather than
                    // string equality: on case-insensitive filesystems the
                    // canonical string preserves caller casing, so the same
                    // directory reached via differently-cased paths must
                    // still adopt this scope instead of splitting.
                    Ok(existing_canonical) if paths_refer_to_same_file(&existing_canonical, &canonical) => {
                        return Some(scope_dir);
                    }
                    Ok(_) => {
                        // Recorded path resolves to a different live clone.
                        continue;
                    }
                    Err(_) => {
                        // Recorded path no longer canonicalizes. That happens
                        // both when the repo was genuinely moved/deleted AND
                        // when the recorded path sits on an unmounted volume.
                        // Only adopt in the former case — adopting while a
                        // sibling clone's volume is merely offline would steal
                        // its scope and permanently cross-wire state between
                        // the two clones.
                        if recorded_path_is_transiently_unavailable(recorded) {
                            continue;
                        }
                        return Some(scope_dir);
                    }
                }
            }
            Err(_) => {
                // No marker → legacy scope, adopt for backwards compat.
                return Some(scope_dir);
            }
        }
    }
    None
}

// A recorded path that fails to canonicalize is either gone for good (the
// repo was moved or deleted — safe to adopt its scope) or only temporarily
// unreachable (its volume is unmounted — adopting would corrupt the absent
// clone's state). The filesystem cannot tell us which, so we use a mount-shape
// heuristic:
//
//   * Walk up the recorded path's ancestors to the first one that exists. If
//     that ancestor is `/` or `/Volumes`, the entire mount point is missing —
//     treat the path as transiently unavailable and skip adoption. If a
//     deeper ancestor exists (the parent chain is present but the leaf is
//     gone), the volume is mounted and the repo itself disappeared — treat it
//     as moved.
//   * Independently, a recorded path under `/Volumes/<name>/` or `/mnt/<name>/`
//     whose volume root does not exist is treated as transiently unavailable,
//     covering Linux-style mounts where `/mnt` itself always exists.
//   * If no ancestor exists at all — impossible on Unix where `/` always
//     exists, but the case on Windows for an offline drive letter (`D:\repo`)
//     or a disconnected UNC share — an absolute path is treated as
//     transiently unavailable.
//
// The cost of guessing wrong is asymmetric: skipping adoption for a truly
// moved repo just creates a fresh scope (recoverable; the old scope stays on
// disk), while adopting an unmounted clone's scope silently shares workflow
// state across clones with no way back.
fn recorded_path_is_transiently_unavailable(recorded: &Path) -> bool {
    if let Some(volume_root) = mount_volume_root(recorded) {
        if !volume_root.exists() {
            return true;
        }
    }

    let mut ancestor = recorded.parent();
    while let Some(current) = ancestor {
        if current.exists() {
            return current == Path::new("/") || current == Path::new("/Volumes");
        }
        ancestor = current.parent();
    }
    recorded.is_absolute()
}

fn mount_volume_root(path: &Path) -> Option<PathBuf> {
    use std::path::Component;
    let mut components = path.components();
    if components.next()? != Component::RootDir {
        return None;
    }
    let Component::Normal(mount_parent) = components.next()? else {
        return None;
    };
    if mount_parent != "Volumes" && mount_parent != "mnt" {
        return None;
    }
    let Component::Normal(volume_name) = components.next()? else {
        return None;
    };
    Some(Path::new("/").join(mount_parent).join(volume_name))
}

fn paths_refer_to_same_file(a: &Path, b: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let (Ok(meta_a), Ok(meta_b)) = (std::fs::metadata(a), std::fs::metadata(b)) {
            return meta_a.dev() == meta_b.dev() && meta_a.ino() == meta_b.ino();
        }
    }
    a == b
}

pub fn sanitize_identifier(value: &str, fallback: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut trailing_separator = false;

    for ch in value.chars() {
        match ch {
            'a'..='z' | 'A'..='Z' | '0'..='9' => {
                out.push(ch.to_ascii_lowercase());
                trailing_separator = false;
            }
            ' ' | '_' | '-' if !out.is_empty() && !trailing_separator => {
                out.push('-');
                trailing_separator = true;
            }
            ' ' | '_' | '-' => {}
            _ => {}
        }
    }

    if trailing_separator {
        out.pop();
    }

    if out.is_empty() {
        fallback.to_string()
    } else {
        out
    }
}

pub fn repository_scope_for_path(path: &Path) -> String {
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let canonical_display = canonical.to_string_lossy();
    let repo_name = canonical
        .file_name()
        .and_then(|value| value.to_str())
        .map(|s| sanitize_identifier(s, "repo"))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "repo".to_string());

    let mut hasher = Sha256::new();
    hasher.update(canonical_display.as_bytes());
    let digest = hasher.finalize();
    let suffix = format!(
        "{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        digest[0], digest[1], digest[2], digest[3], digest[4], digest[5]
    );
    format!("{repo_name}-{suffix}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::EnvVarGuard;
    use proptest::prelude::*;
    use tempfile::tempdir;

    #[test]
    fn sanitize_identifier_normalizes_expected_shapes() {
        assert_eq!(sanitize_identifier("Repo Name", "repo"), "repo-name");
        assert_eq!(sanitize_identifier("___", "repo"), "repo");
        assert_eq!(sanitize_identifier("A__B--C", "repo"), "a-b-c");
        assert_eq!(sanitize_identifier("  __My Repo!! -- 2026__  ", "repo"), "my-repo-2026");
        assert_eq!(sanitize_identifier("日本語", "repo"), "repo");
        assert_eq!(sanitize_identifier("日本語", "task"), "task");
    }

    #[test]
    fn repository_scope_for_path_uses_canonical_basename() {
        let root = tempfile::tempdir().expect("tempdir");
        let canonical = root.path().join("Canonical Repo");
        std::fs::create_dir_all(&canonical).expect("create canonical path");
        let alias = root.path().join("alias");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&canonical, &alias).expect("create symlink");
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(&canonical, &alias).expect("create symlink");

        let scope = repository_scope_for_path(&alias);
        assert!(scope.starts_with("canonical-repo-"));
    }

    #[test]
    fn repository_scope_for_path_emits_slug_and_12_hex_suffix() {
        let temp = tempfile::tempdir().expect("tempdir");
        let scope = repository_scope_for_path(temp.path());
        let (slug, suffix) = scope.rsplit_once('-').expect("scope should contain hyphen");
        assert!(!slug.is_empty());
        assert_eq!(suffix.len(), 12);
        assert!(suffix.chars().all(|ch| ch.is_ascii_hexdigit()));
        assert_eq!(suffix, suffix.to_ascii_lowercase());
    }

    #[test]
    fn repository_scope_for_path_uses_raw_path_when_canonicalize_fails() {
        let temp = tempfile::tempdir().expect("tempdir");
        let missing = temp.path().join("Missing Repo 2026");

        let scope = repository_scope_for_path(&missing);
        assert!(scope.starts_with("missing-repo-2026-"));
    }

    #[test]
    fn scoped_state_root_avoids_git_lookup_when_scope_already_exists() {
        let temp = tempdir().expect("tempdir");
        let home = temp.path().join("home");
        let repo = temp.path().join("repo");
        let bin = temp.path().join("bin");
        let marker = temp.path().join("git-called");
        std::fs::create_dir_all(home.join(".animus")).expect("ao root");
        std::fs::create_dir_all(&repo).expect("repo root");
        std::fs::create_dir_all(&bin).expect("bin dir");

        let scope_dir = home.join(".animus").join(repository_scope_for_path(&repo));
        std::fs::create_dir_all(&scope_dir).expect("scope dir");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let git_script = bin.join("git");
            std::fs::write(&git_script, format!("#!/bin/sh\ntouch '{}'\nexit 1\n", marker.display()))
                .expect("write fake git");
            let mut perms = std::fs::metadata(&git_script).expect("metadata").permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&git_script, perms).expect("set perms");
        }

        let _home_guard = EnvVarGuard::set("HOME", Some(home.to_string_lossy().as_ref()));
        let _path_guard = EnvVarGuard::set("PATH", Some(bin.to_string_lossy().as_ref()));

        let resolved = scoped_state_root(&repo).expect("scope dir");
        assert_eq!(resolved, scope_dir);
        assert!(!marker.exists(), "existing scope lookup should not invoke git");
    }

    #[cfg(unix)]
    #[test]
    fn scoped_state_root_isolates_distinct_clones_with_same_origin() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempdir().expect("tempdir");
        let home = temp.path().join("home");
        let bin = temp.path().join("bin");
        let clone_a = temp.path().join("clones").join("alpha");
        let clone_b = temp.path().join("clones").join("beta");
        std::fs::create_dir_all(home.join(".animus")).expect("ao root");
        std::fs::create_dir_all(&clone_a).expect("clone a");
        std::fs::create_dir_all(&clone_b).expect("clone b");
        std::fs::create_dir_all(&bin).expect("bin dir");

        // Fake git that always reports the same origin URL regardless of cwd.
        let git_script = bin.join("git");
        std::fs::write(&git_script, "#!/bin/sh\necho 'git@github.com:example/shared-repo.git'\n")
            .expect("write fake git");
        let mut perms = std::fs::metadata(&git_script).expect("metadata").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&git_script, perms).expect("set perms");

        let _home_guard = EnvVarGuard::set("HOME", Some(home.to_string_lossy().as_ref()));
        let _path_guard = EnvVarGuard::set("PATH", Some(bin.to_string_lossy().as_ref()));

        let scope_a = scoped_state_root(&clone_a).expect("scope a");
        let scope_b = scoped_state_root(&clone_b).expect("scope b");

        let expected_a = home.join(".animus").join(repository_scope_for_path(&clone_a));
        let expected_b = home.join(".animus").join(repository_scope_for_path(&clone_b));

        assert_eq!(scope_a, expected_a, "clone A should land on its hash-derived scope");
        assert_eq!(scope_b, expected_b, "clone B should land on its hash-derived scope");
        assert_ne!(scope_a, scope_b, "two clones of the same origin must not share a scope");

        // Subsequent calls must remain stable and not cross over via the
        // same-origin fallback.
        let scope_a_again = scoped_state_root(&clone_a).expect("scope a again");
        let scope_b_again = scoped_state_root(&clone_b).expect("scope b again");
        assert_eq!(scope_a_again, expected_a);
        assert_eq!(scope_b_again, expected_b);

        // Markers should record each clone's own canonical path.
        let marker_a = std::fs::read_to_string(scope_a.join(".project-root")).expect("marker a");
        let marker_b = std::fs::read_to_string(scope_b.join(".project-root")).expect("marker b");
        let canonical_a = clone_a.canonicalize().expect("canon a");
        let canonical_b = clone_b.canonicalize().expect("canon b");
        assert_eq!(marker_a.trim(), canonical_a.to_string_lossy());
        assert_eq!(marker_b.trim(), canonical_b.to_string_lossy());
    }

    #[cfg(unix)]
    #[test]
    fn paths_refer_to_same_file_follows_symlinks_and_rejects_distinct_dirs() {
        let temp = tempdir().expect("tempdir");
        let real = temp.path().join("real");
        let other = temp.path().join("other");
        let link = temp.path().join("link");
        std::fs::create_dir_all(&real).expect("real");
        std::fs::create_dir_all(&other).expect("other");
        std::os::unix::fs::symlink(&real, &link).expect("symlink");

        assert!(paths_refer_to_same_file(&real, &link));
        assert!(!paths_refer_to_same_file(&real, &other));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn scoped_state_root_adopts_existing_scope_for_differently_cased_path() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempdir().expect("tempdir");
        let home = temp.path().join("home");
        let bin = temp.path().join("bin");
        let repo = temp.path().join("clones").join("MixedCase");
        std::fs::create_dir_all(home.join(".animus")).expect("ao root");
        std::fs::create_dir_all(&repo).expect("repo");
        std::fs::create_dir_all(&bin).expect("bin dir");

        let git_script = bin.join("git");
        std::fs::write(&git_script, "#!/bin/sh\necho 'git@github.com:example/cased-repo.git'\n")
            .expect("write fake git");
        let mut perms = std::fs::metadata(&git_script).expect("metadata").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&git_script, perms).expect("set perms");

        let _home_guard = EnvVarGuard::set("HOME", Some(home.to_string_lossy().as_ref()));
        let _path_guard = EnvVarGuard::set("PATH", Some(bin.to_string_lossy().as_ref()));

        let original_scope = scoped_state_root(&repo).expect("original scope");

        let lowercased = temp.path().join("clones").join("mixedcase");
        if !lowercased.exists() {
            // Case-sensitive volume — the differently-cased path is a
            // different file and the scenario does not apply.
            return;
        }
        let adopted = scoped_state_root(&lowercased).expect("adopted scope");
        assert_eq!(adopted, original_scope, "differently-cased access must adopt the existing scope, not split");
    }

    #[cfg(unix)]
    #[test]
    fn scoped_state_root_adopts_moved_clone_when_recorded_path_missing() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempdir().expect("tempdir");
        let home = temp.path().join("home");
        let bin = temp.path().join("bin");
        let new_clone = temp.path().join("new-location");
        std::fs::create_dir_all(home.join(".animus")).expect("ao root");
        std::fs::create_dir_all(&new_clone).expect("new clone");
        std::fs::create_dir_all(&bin).expect("bin dir");

        let git_script = bin.join("git");
        std::fs::write(&git_script, "#!/bin/sh\necho 'git@github.com:example/moved-repo.git'\n")
            .expect("write fake git");
        let mut perms = std::fs::metadata(&git_script).expect("metadata").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&git_script, perms).expect("set perms");

        // Pre-create a legacy scope whose recorded `.project-root` no longer exists.
        let legacy_scope = home.join(".animus").join("legacy-scope-aaaaaaaaaaaa");
        std::fs::create_dir_all(&legacy_scope).expect("legacy scope");
        std::fs::write(legacy_scope.join(".git-origin"), "git@github.com:example/moved-repo.git\n")
            .expect("write origin");
        std::fs::write(legacy_scope.join(".project-root"), format!("{}\n", temp.path().join("old-location").display()))
            .expect("write stale project-root");

        let _home_guard = EnvVarGuard::set("HOME", Some(home.to_string_lossy().as_ref()));
        let _path_guard = EnvVarGuard::set("PATH", Some(bin.to_string_lossy().as_ref()));

        let resolved = scoped_state_root(&new_clone).expect("scope");
        assert_eq!(resolved, legacy_scope, "moved clone should reclaim its legacy scope");

        // And the marker should now point at the new canonical location.
        let marker = std::fs::read_to_string(legacy_scope.join(".project-root")).expect("marker");
        let canonical_new = new_clone.canonicalize().expect("canon");
        assert_eq!(marker.trim(), canonical_new.to_string_lossy());
    }

    #[test]
    fn recorded_path_transience_distinguishes_missing_mount_from_missing_leaf() {
        assert!(recorded_path_is_transiently_unavailable(Path::new("/Volumes/animus-no-such-volume-1f2e3d/repo")));
        assert!(recorded_path_is_transiently_unavailable(Path::new("/mnt/animus-no-such-volume-1f2e3d/repo")));
        // Repo mounted directly at the volume root.
        assert!(recorded_path_is_transiently_unavailable(Path::new("/mnt/animus-no-such-volume-1f2e3d")));
        assert!(recorded_path_is_transiently_unavailable(Path::new("/animus-no-such-root-1f2e3d/nested/repo")));

        // Parent chain present, leaf gone → the repo was moved or deleted.
        let temp = tempdir().expect("tempdir");
        let leaf_gone = temp.path().join("removed-leaf");
        assert!(!recorded_path_is_transiently_unavailable(&leaf_gone));
    }

    #[cfg(unix)]
    #[test]
    fn scoped_state_root_skips_adoption_when_recorded_path_on_absent_mount() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempdir().expect("tempdir");
        let home = temp.path().join("home");
        let bin = temp.path().join("bin");
        let new_clone = temp.path().join("home-clone");
        std::fs::create_dir_all(home.join(".animus")).expect("ao root");
        std::fs::create_dir_all(&new_clone).expect("new clone");
        std::fs::create_dir_all(&bin).expect("bin dir");

        let git_script = bin.join("git");
        std::fs::write(&git_script, "#!/bin/sh\necho 'git@github.com:example/unmounted-repo.git'\n")
            .expect("write fake git");
        let mut perms = std::fs::metadata(&git_script).expect("metadata").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&git_script, perms).expect("set perms");

        // Pre-existing scope owned by a clone on a volume that is not mounted.
        let unmounted_root = "/Volumes/animus-test-absent-volume-9a8b7c/repo";
        let offline_scope = home.join(".animus").join("offline-scope-bbbbbbbbbbbb");
        std::fs::create_dir_all(&offline_scope).expect("offline scope");
        std::fs::write(offline_scope.join(".git-origin"), "git@github.com:example/unmounted-repo.git\n")
            .expect("write origin");
        std::fs::write(offline_scope.join(".project-root"), format!("{unmounted_root}\n")).expect("write project-root");

        let _home_guard = EnvVarGuard::set("HOME", Some(home.to_string_lossy().as_ref()));
        let _path_guard = EnvVarGuard::set("PATH", Some(bin.to_string_lossy().as_ref()));

        let resolved = scoped_state_root(&new_clone).expect("scope");
        let expected = home.join(".animus").join(repository_scope_for_path(&new_clone));
        assert_eq!(resolved, expected, "clone must not adopt a scope whose owner is merely unmounted");
        assert_ne!(resolved, offline_scope);

        // The offline clone's marker must survive untouched for remount.
        let marker = std::fs::read_to_string(offline_scope.join(".project-root")).expect("marker");
        assert_eq!(marker.trim(), unmounted_root);
    }

    #[cfg(unix)]
    #[test]
    fn scoped_state_root_fast_path_reclaims_marker_pointing_at_other_live_clone() {
        use std::io::Write;
        use std::sync::{Arc, Mutex};

        #[derive(Clone, Default)]
        struct CaptureWriter(Arc<Mutex<Vec<u8>>>);

        impl Write for CaptureWriter {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().expect("capture lock").extend_from_slice(buf);
                Ok(buf.len())
            }

            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CaptureWriter {
            type Writer = CaptureWriter;

            fn make_writer(&'a self) -> Self::Writer {
                self.clone()
            }
        }

        let temp = tempdir().expect("tempdir");
        let home = temp.path().join("home");
        let other_clone = temp.path().join("clones").join("external");
        let our_clone = temp.path().join("clones").join("local");
        std::fs::create_dir_all(home.join(".animus")).expect("ao root");
        std::fs::create_dir_all(&other_clone).expect("other clone");
        std::fs::create_dir_all(&our_clone).expect("our clone");

        // Our hash-derived scope exists but its marker was rewritten by a
        // sibling clone while our path was unreachable.
        let scope_dir = home.join(".animus").join(repository_scope_for_path(&our_clone));
        std::fs::create_dir_all(&scope_dir).expect("scope dir");
        let canonical_other = other_clone.canonicalize().expect("canon other");
        std::fs::write(scope_dir.join(".project-root"), format!("{}\n", canonical_other.display()))
            .expect("write foreign marker");

        let _home_guard = EnvVarGuard::set("HOME", Some(home.to_string_lossy().as_ref()));

        let capture = CaptureWriter::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(capture.clone())
            .with_max_level(tracing::Level::WARN)
            .with_ansi(false)
            .finish();

        let resolved = tracing::subscriber::with_default(subscriber, || scoped_state_root(&our_clone).expect("scope"));
        assert_eq!(resolved, scope_dir);

        // The marker must be reclaimed for the caller whose path hashes to
        // this scope dir's name.
        let marker = std::fs::read_to_string(scope_dir.join(".project-root")).expect("marker");
        let canonical_ours = our_clone.canonicalize().expect("canon ours");
        assert_eq!(marker.trim(), canonical_ours.to_string_lossy());

        let logs = String::from_utf8(capture.0.lock().expect("capture lock").clone()).expect("utf8 logs");
        assert!(logs.contains("reclaiming the scope"), "expected a warn about reclaiming, got: {logs}");
        assert!(logs.contains(canonical_other.to_string_lossy().as_ref()), "warn should name the recorded path");
        assert!(logs.contains(canonical_ours.to_string_lossy().as_ref()), "warn should name the current path");
    }

    #[cfg(unix)]
    #[test]
    fn scoped_state_root_fast_path_keeps_matching_marker_untouched() {
        let temp = tempdir().expect("tempdir");
        let home = temp.path().join("home");
        let repo = temp.path().join("repo");
        std::fs::create_dir_all(home.join(".animus")).expect("ao root");
        std::fs::create_dir_all(&repo).expect("repo");

        let scope_dir = home.join(".animus").join(repository_scope_for_path(&repo));
        std::fs::create_dir_all(&scope_dir).expect("scope dir");
        let canonical = repo.canonicalize().expect("canon");
        let marker_body = format!("{}\n", canonical.display());
        std::fs::write(scope_dir.join(".project-root"), &marker_body).expect("write marker");

        let _home_guard = EnvVarGuard::set("HOME", Some(home.to_string_lossy().as_ref()));

        let resolved = scoped_state_root(&repo).expect("scope");
        assert_eq!(resolved, scope_dir);
        let marker = std::fs::read_to_string(scope_dir.join(".project-root")).expect("marker");
        assert_eq!(marker, marker_body);
    }

    proptest! {
        #[test]
        fn sanitize_identifier_output_contains_only_valid_chars(input in "\\PC*") {
            let result = sanitize_identifier(&input, "fallback");
            prop_assert!(result.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '-'));
            prop_assert!(!result.is_empty());
            prop_assert!(!result.starts_with('-'));
            prop_assert!(!result.ends_with('-'));
        }

        #[test]
        fn sanitize_identifier_is_idempotent(input in "\\PC*") {
            let once = sanitize_identifier(&input, "fallback");
            let twice = sanitize_identifier(&once, "fallback");
            prop_assert_eq!(once, twice);
        }

        #[test]
        fn repository_scope_for_path_never_panics(input in "\\PC{1,200}") {
            let path = std::path::Path::new(&input);
            let _scope = repository_scope_for_path(path);
        }
    }
}
