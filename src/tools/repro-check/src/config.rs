use std::fs;
use std::path::Path;

use anyhow::Result;
use log::info;

pub fn write_bootstrap_toml(env_root: &Path, target: Option<&str>) -> Result<()> {
    let target_line = target.map(|t| format!("target = [\"{}\"]\n", t)).unwrap_or_default();

    let content = format!(
        r#"
[llvm]
download-ci-llvm = true

[build]
optimized-compiler-builtins = true
{}

[target.x86_64-pc-windows-msvc]
cc = "clang-cl.exe"

[rust]
remap-debuginfo = true
debug = false
debug-assertions = false
backtrace-on-ice = false
debug-logging = false
channel = "nightly"
debuginfo-level = 1

[dist]
src-tarball = false
"#,
        target_line
    );

    let toml_path = env_root.join("bootstrap.toml");
    fs::write(&toml_path, content.trim_start())?;
    info!("Wrote deterministic bootstrap.toml to {}", toml_path.display());
    Ok(())
}
