use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use log::{info, warn};

// Sets up two build envs by copying source (if not skipped), cleaning first if asked.
// The path_delta is for testing path sensitivity - adds extra dir levels to B.
pub fn prepare_workspace(
    workspace: &Path,
    src_root: &Path,
    path_delta: usize,
    skip_copy: bool,
) -> Result<(PathBuf, PathBuf)> {
    if !skip_copy {
        info!("Setting up fresh workspace at {:?}", workspace);
        clean_workspace(workspace)?;
        fs::create_dir_all(workspace)?;
    }

    let env_a = workspace.join("build-a");
    let mut env_b = workspace.join("build-b");

    if path_delta > 0 {
        for i in 1..=path_delta {
            env_b = env_b.join(format!("extra{}", i));
        }
    }

    if cfg!(windows) && path_delta > 10 {
        warn!("Watch out - long paths on Windows might need registry tweaks.");
    }

    if !skip_copy {
        info!("Cloning sources to A: {:?}", env_a);
        clone_source_tree(src_root, &env_a)?;

        info!("Cloning sources to B: {:?}", env_b);
        clone_source_tree(src_root, &env_b)?;
    }

    Ok((env_a, env_b))
}

pub fn clean_workspace(workspace: &Path) -> Result<()> {
    if workspace.exists() {
        info!("Cleaning up old workspace: {:?}", workspace);
        fs::remove_dir_all(workspace).context("Workspace clean failed")?;
    }
    Ok(())
}

// Creates an independent checkout with its own Git index and worktree.
// The main repository shares Git objects with the source for CI efficiency.
// Submodules are initialized independently.
fn clone_source_tree(src: &Path, dest: &Path) -> Result<()> {
    let head = std::process::Command::new("git")
        .arg("-C")
        .arg(src)
        .args(["rev-parse", "--verify", "HEAD"])
        .output()
        .context("Failed to resolve source HEAD")?;

    anyhow::ensure!(
        head.status.success(),
        "Failed to resolve source HEAD: {}",
        String::from_utf8_lossy(&head.stderr)
    );

    let commit = String::from_utf8(head.stdout)?;
    let commit = commit.trim();

    let status = std::process::Command::new("git")
        .args(["clone", "--shared", "--no-checkout", "--"])
        .arg(src)
        .arg(dest)
        .status()
        .context("Failed to clone source repository")?;

    anyhow::ensure!(status.success(), "Git clone failed for {}", dest.display());

    let status = std::process::Command::new("git")
        .arg("-C")
        .arg(dest)
        .args(["checkout", "--detach", commit])
        .status()
        .context("Failed to check out source revision")?;

    anyhow::ensure!(status.success(), "Checkout failed for {}", dest.display());

    let status = std::process::Command::new("git")
        .arg("-C")
        .arg(dest)
        .args(["submodule", "update", "--init", "--recursive", "--jobs", "4"])
        .status()
        .context("Failed to initialize submodules")?;

    anyhow::ensure!(
        status.success(),
        "Submodule initialization failed for {}",
        dest.display()
    );

    // Verify that Git operates on the new checkout, not the source.
    for relative in [".", "src/llvm-project"] {
        let path = dest.join(relative);
        let expected = fs::canonicalize(&path)
            .with_context(|| format!("Missing checkout directory: {}", path.display()))?;

        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(&path)
            .args(["rev-parse", "--show-toplevel"])
            .output()
            .context("Failed to verify checkout worktree")?;

        anyhow::ensure!(
            output.status.success(),
            "Invalid Git checkout at {}: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr)
        );

        let actual = fs::canonicalize(String::from_utf8(output.stdout)?.trim())?;

        anyhow::ensure!(
            actual == expected,
            "Wrong Git worktree: expected {}, got {}",
            expected.display(),
            actual.display()
        );
    }

    Ok(())
}
