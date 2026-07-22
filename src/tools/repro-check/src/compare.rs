use std::collections::{HashSet, HashMap};
use std::fs::File;
use std::io::copy;
use std::path::{Path, PathBuf};

use anyhow::Result;
use chrono::Utc;
use log::{info, trace, warn};
use rayon::prelude::*;
use sha2::{Digest, Sha256};
use walkdir::{DirEntry, WalkDir};
use cargo_metadata::MetadataCommand;
use object::{Object, ObjectSection};
use std::process::Command;

#[derive(Debug, Clone)]
pub struct ComparisonReport {
    pub mismatches: Vec<Mismatch>,
    pub total_files: usize,
    pub matching_files: usize,
    pub ignored_files: Vec<(PathBuf, String)>,
    pub compared_files: Vec<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct Mismatch {
    pub path: PathBuf,
    pub hash_a: String,
    pub hash_b: String,
    pub diffoscope_output: Option<String>,
    pub zls_root_output: Option<ZlsRootReport>,
    pub root_cause_crates: Vec<String>, 
    pub dwarf_path_diff: Option<DwarfPathDiff>,
    pub normalized_hash_a: Option<String>,
    pub normalized_hash_b: Option<String>,
    pub normalized_match: Option<bool>,
    pub normalization_notes: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct CrateEntry {
    pub name: String,
    pub hash_a: String,
    pub hash_b: String,
    pub kind: String,
    pub linkage: String,
    pub hash_match: bool,
}

#[derive(Debug, Clone)]
pub struct ZlsRootReport {
    pub crates: Vec<CrateEntry>,
    pub status: String,
}

#[derive(Debug, Clone)]
pub struct DwarfPathDiff {
    pub only_in_a: Vec<String>,
    pub only_in_b: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct PeNormalizationResult {
    pub normalized_hash: String,
    pub notes: Vec<String>,
}

/// Compares two directories, ignoring certain file patterns.
/// Collects files from dir_a, filters them, hashes in parallel, then checks against dir_b.
/// We sort entries for consistent ordering - helps with debugging.

pub fn diffoscope_diff (path_a: &Path, path_b: &Path) -> Option<String> {
    let output = std::process::Command::new("diffoscope")
        .arg("--text-color=never")
        .arg(path_a)
        .arg(path_b)
        .output()
        .ok()?;
    
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    if stdout.is_empty() && stderr.is_empty() {
        None
    } else {
        Some(format!("{}{}", stdout, stderr))
    }
}

pub fn zls_root_diff (path_a: &Path, path_b: &Path, rustc_a: &Path, rustc_b: &Path) -> Option<ZlsRootReport> {
    let out_a = match Command::new(rustc_a)
    .arg("-Zls=root")
    .arg(path_a)
    .output()
    {
        Ok(output) => output,
        Err(err) => {
            return Some(ZlsRootReport {
                crates: vec![],
                status: format!("command failed: {err}"),
            });
        }
    };

    let out_b = match Command::new(rustc_b)
    .arg("-Zls=root")
    .arg(path_b)
    .output()
    {
        Ok(output) => output,
        Err(err) => {
            return Some(ZlsRootReport {
                crates: vec![],
                status: format!("command failed: {err}"),
            });
        }
    };
    if !out_a.status.success() || !out_b.status.success() {
        return Some(ZlsRootReport {
            crates: vec![],
            status: format!(
                "rustc failed:\nA: {}\nB: {}",
                String::from_utf8_lossy(&out_a.stderr),
                String::from_utf8_lossy(&out_b.stderr),
            ),
        });
    }

    let text_a = String::from_utf8_lossy(&out_a.stdout).to_string();
    let text_b = String::from_utf8_lossy(&out_b.stdout).to_string();
     
    let crates_a = parse_zls_output(&text_a);
    let crates_b = parse_zls_output(&text_b);
    
    let mut crates = Vec::new();
    for entry_a in crates_a {
        if let Some(entry_b) = crates_b.iter().find(|c| c.name == entry_a.name) {
            let hash_match = entry_a.hash_a == entry_b.hash_a;
            let entry = CrateEntry {
                name: entry_a.name.clone(),
                hash_a: entry_a.hash_a.clone(),
                hash_b: entry_b.hash_a.clone(),
                kind: entry_a.kind.clone(),
                linkage: entry_a.linkage.clone(),
                hash_match,
            };
            crates.push(entry);
        }
    }
    Some(ZlsRootReport { crates, status: "ok".to_string(), })
    
    
}

pub fn parse_zls_output(text: &str) -> Vec<CrateEntry> {
    let mut crates = Vec::new();
    for line in text.lines() {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        if tokens.len() < 11 {
            continue;
        }
        if tokens[0].parse::<u32>().is_err() {
            continue;
        }
        crates.push(CrateEntry {
            name: tokens[1].to_string(),
            hash_a: tokens[3].to_string(),
            hash_b: tokens[3].to_string(),
            kind: tokens[7].to_string(),
            linkage: tokens[10].to_string(),
            hash_match: true,
        });
    }
    crates
}

pub fn find_root_cause_crates(zls_report: &ZlsRootReport, src_root: &Path) -> Vec<String> {
    let metadata = match MetadataCommand::new()
        .manifest_path(src_root.join("Cargo.toml"))
        .exec()
    {
        Ok(m) => m,
        Err(e) => {
            warn!("cargo metadata failed: {}", e);
            return vec![];
        }
    };

    let map: HashMap<String, Vec<String>> = metadata.packages.iter()
    .map(|pkg| {
        let deps = pkg.dependencies.iter()
            .map(|d| d.name.replace('-', "_"))
            .collect();
        (pkg.name.to_string().replace('-', "_"), deps)
    })
    .collect();

    let differing_set: HashSet<String> = zls_report.crates.iter()
        .filter(|c| !c.hash_match)
        .map(|c| c.name.rsplitn(2, '-').last().unwrap_or(&c.name).to_string())
        .collect();

    let mut results = Vec::new();

    for c in zls_report.crates.iter().filter(|c| !c.hash_match) {
       let base_name = c.name.rsplitn(2, '-').last().unwrap_or(&c.name);
        let deps = match map.get(base_name) {
            Some(d) => d,
            None => continue,
        };
        let has_differing_dep = deps.iter()
            .any(|d| differing_set.contains(d));
            info!("checking {} deps={:?} differing_dep={}", base_name, deps, has_differing_dep);
        if !has_differing_dep {
            results.push(base_name.to_string());
        }
    }
    results
}

pub fn dwarf_file_paths(binary: &Path) -> anyhow::Result<Vec<String>> {
    let data = std::fs::read(binary)?;
    let obj = object::File::parse(&*data)?;
    
    let endian = if obj.is_little_endian() {
        gimli::RunTimeEndian::Little
    } else {
        gimli::RunTimeEndian::Big
    };
    
    let load_section = |id: gimli::SectionId| -> anyhow::Result<_> {
        let data = obj.section_by_name(id.name())
            .and_then(|s| s.data().ok())
            .unwrap_or(&[]);
        Ok(gimli::EndianSlice::new(data, endian))
    };
    
    let dwarf = gimli::Dwarf::load(load_section)?;
    let mut paths = Vec::new();
    
    let mut iter = dwarf.units();
    while let Some(header) = iter.next()? {
        let unit = dwarf.unit(header)?;
        if let Some(program) = unit.line_program.clone() {
            let header = program.header();
            let dirs: Vec<String> = header
                .include_directories()
                .iter()
                .map(|dir| {
                    dwarf
                        .attr_string(&unit, *dir)
                        .map(|d| d.to_string_lossy().into_owned())
                        .unwrap_or_default()
                })
                .collect();

            for file in header.file_names() {
                let file_name = dwarf
                    .attr_string(&unit, file.path_name())?
                    .to_string_lossy()
                    .into_owned();

                let idx = file.directory_index();

                let dir = if idx > 0 {
                    dirs.get((idx - 1) as usize)
                        .cloned()
                        .unwrap_or_default()
                } else {
                    String::new()
                };

                let full_path = if dir.is_empty() {
                    file_name
                } else {
                    format!("{}/{}", dir, file_name)
                };

                paths.push(full_path);
            }
        }
    }
    Ok(paths)
}

fn normalize_and_filter(path: &str) -> Option<String> {
    let interesting = ["compiler/", "library/"];
    let pos = interesting.iter().find_map(|key| path.find(key))?;

    let trimmed = &path[pos..];

    Some(trimmed.to_string())
}

pub fn diff_dwarf_paths(path_a: &Path, path_b: &Path) -> Option<DwarfPathDiff> {
    let raw_a = dwarf_file_paths(path_a).ok()?;
    let raw_b = dwarf_file_paths(path_b).ok()?;

    let paths_a: HashSet<String> = raw_a
        .iter()
        .filter_map(|p| normalize_and_filter(&p))
        .collect();

    let paths_b: HashSet<String> = raw_b
        .iter()
        .filter_map(|p| normalize_and_filter(&p))
        .collect();
 
    if paths_a.is_empty() && paths_b.is_empty() {
        return None;
    }

    let only_in_a: Vec<String> = paths_a.difference(&paths_b).cloned().collect();
    let only_in_b: Vec<String> = paths_b.difference(&paths_a).cloned().collect();

    if only_in_a.is_empty() && only_in_b.is_empty() {
        return None;
    }

    Some(DwarfPathDiff { only_in_a, only_in_b })
}

pub fn compare_directories(
    dir_a: &Path,
    dir_b: &Path,
    host: &str,
    exclude_patterns: &HashSet<String>,
    run_diffoscope: bool,
    src_root: &Path
) -> Result<ComparisonReport> {
    let mut entries_a: Vec<DirEntry> = WalkDir::new(dir_a)
        .sort_by_file_name()
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .collect();

    let mut ignored_files = Vec::new();
    let mut compared_files = Vec::new();

    entries_a.retain(|entry| {
        let fname = entry.file_name().to_string_lossy().to_string();

        // Always compare lowercase for case-insensitive suffix match
        let name_to_check = fname.to_lowercase();

        for pat in exclude_patterns {
            let pat_to_check = pat.to_lowercase();

            if name_to_check.ends_with(&pat_to_check) {
                let rel = entry.path().strip_prefix(dir_a).unwrap().to_path_buf();
                ignored_files.push((rel, pat.clone()));
                return false;
            }
        }

        let rel = entry.path().strip_prefix(dir_a).unwrap().to_path_buf();
        compared_files.push(rel);
        true
    });

    let total_files = entries_a.len() + ignored_files.len();
    trace!("Found {} files to compare, ignored {}", entries_a.len(), ignored_files.len());

    let hashes_a: Vec<(PathBuf, String)> = entries_a
        .par_iter()
        .map(|entry| {
            let rel_path = entry.path().strip_prefix(dir_a).unwrap().to_path_buf();
            match compute_hash(entry.path()) {
                Ok(h) => (rel_path, h),
                Err(e) => {
                    warn!("Hash error on {:?}: {}", entry.path(), e);
                    (rel_path, "HASH_ERROR".to_string())
                }
            }
        })
        .collect();

    let mut mismatches = Vec::new();
    for (rel_path, hash_a) in hashes_a {
        let path_b = dir_b.join(&rel_path);
        let hash_b = if path_b.exists() {
            compute_hash(&path_b)
                .map_err(|e| warn!("Hash fail on B {:?}: {}", path_b, e))
                .unwrap_or("HASH_ERROR".to_string())
        } else {
            "MISSING_FILE".to_string()
        };

                if hash_a != hash_b {
        let diffoscope_output = if run_diffoscope {
            let full_path_a = dir_a.join(&rel_path);
            let full_path_b = dir_b.join(&rel_path);
            diffoscope_diff(&full_path_a, &full_path_b)
        } else {
            None
        };

        let zls_root_output = zls_root_diff(
            &dir_a.join(&rel_path),
            &dir_b.join(&rel_path),
            &dir_a.join("bin/rustc"),
            &dir_b.join("bin/rustc"),
        );
        let root_cause_crates = if let Some(ref zls) = zls_root_output {
            find_root_cause_crates(zls, src_root)
        } else {
            vec![]
        };
        let dwarf_path_diff = diff_dwarf_paths(&dir_a.join(&rel_path), &dir_b.join(&rel_path));

        let mut normalized_hash_a = None;
        let mut normalized_hash_b = None;
        let mut normalized_match = None;
        let mut normalization_notes = Vec::new();

        // --- Windows PE normalization ---
        #[cfg(windows)]
        {
            let is_pe = rel_path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.eq_ignore_ascii_case("exe") || e.eq_ignore_ascii_case("dll"))
                .unwrap_or(false);

            if is_pe {
                if let (Ok(norm_a), Ok(norm_b)) = (
                    compute_normalized_pe_hash(&dir_a.join(&rel_path)),
                    compute_normalized_pe_hash(&dir_b.join(&rel_path)),
                ) {
                        info!("  Norm A = {:?}", norm_a.normalized_hash);
                        info!("  Norm B = {:?}", norm_b.normalized_hash);
                    normalized_match =
                        Some(norm_a.normalized_hash == norm_b.normalized_hash);
                        info!("normalized file: {:?}", rel_path);

                        for note in &norm_a.notes {
                            info!("  A: {}", note);
                        }

                        for note in &norm_b.notes {
                            info!("  B: {}", note);
                        }
                    normalized_hash_a = Some(norm_a.normalized_hash);
                    normalized_hash_b = Some(norm_b.normalized_hash);
                    normalization_notes.extend(norm_a.notes);

                    if normalized_match == Some(true) {
                        continue;
                    }
                }
            }
        }

        mismatches.push(Mismatch {
            path: rel_path,
            hash_a,
            hash_b,
            diffoscope_output,
            zls_root_output,
            root_cause_crates,
            dwarf_path_diff,
            normalized_hash_a,
            normalized_hash_b,
            normalized_match,
            normalization_notes,
        });
    } 
            }

            let matching_files = compared_files.len() - mismatches.len();
            info!("Compared on host {} - mismatches: {}", host, mismatches.len());

                Ok(ComparisonReport {
                    mismatches,
                    total_files,
                    matching_files,
                    ignored_files,
                    compared_files,
                })
            }   

/// Builds an HTML report from the comparison results.
pub fn generate_html_report(report: &ComparisonReport, output_path: &Path) -> Result<()> {
    let (status_class, status_text) =
        if report.mismatches.is_empty() { ("success", "PASSED") } else { ("failure", "FAILED") };

    let timestamp = Utc::now().format("%Y-%m-%d %H:%M:%S UTC").to_string();
    let mut html = String::new();

    html.push_str(&format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="utf-8">
    <title>Repro Check Report</title>
    <style>
        body {{ font-family: monospace; margin: 2rem; background: #f8f9fa; }}
        .container {{ max-width: 80rem; margin: auto; }}
        .header {{ padding: 1.5rem; background: #e9ecef; border-radius: 0.5rem; margin-bottom: 1.5rem; }}
        h1 {{ margin: 0; font-size: 1.8rem; }}
        .success h1 {{ color: green; }}
        .failure h1 {{ color: red; }}
        .summary {{ font-size: 1rem; margin: 0.75rem 0; }}
        table {{ width: 100%; border-collapse: collapse; margin: 1.25rem 0; background: white; }}
        th, td {{ border: 1px solid #dee2e6; padding: 0.625rem; text-align: left; }}
        th {{ background: #f8f9fa; }}
        .mismatch {{ background: #ffe5e5; }}
        .ignored {{ background: #fff3cd; }}
        .section {{ margin: 2rem 0; }}
        .count {{ padding: 0.25rem 0.5rem; border-radius: 0.25rem; font-weight: bold; }}
        .count.match {{ background: #d4edda; color: green; }}
        .count.mismatch {{ background: #f8d7da; color: red; }}
        .count.ignored {{ background: #fff3cd; color: orange; }}
        details {{ margin: 1rem 0; }}
        summary {{ cursor: pointer; font-weight: bold; }}
    </style>
</head>
<body>
<div class="container">
    <div class="header {status_class}">
        <h1>Repro Check: {status_text}</h1>
        <div class="summary">
            <strong>Total files:</strong> {total} |
            <span class="count match">Matching: {matching}</span> |
            <span class="count mismatch">Mismatches: {mcount}</span> |
            <span class="count ignored">Ignored: {icount}</span>
        </div>
    </div>

    <div class="section">
        <h2>Mismatches ({mcount})</h2>"#,
        status_class = status_class,
        status_text = status_text,
        total = report.total_files,
        matching = report.matching_files,
        mcount = report.mismatches.len(),
        icount = report.ignored_files.len(),
    ));

    if report.mismatches.is_empty() {
        html.push_str("<p>Everything matches - good job!</p>");
    } else {
        html.push_str(r#"
            <table>
                <thead><tr><th>File Path</th><th>Hash A (short)</th><th>Hash B (short)</th></tr></thead>
                <tbody>
        "#);
        for mismatch in &report.mismatches {
            let short_a = mismatch.hash_a.get(..16).unwrap_or("N/A");
            let short_b = mismatch.hash_b.get(..16).unwrap_or("N/A");
            html.push_str(&format!(
                r#"<tr class="mismatch"><td>{}</td><td>{}</td><td>{}</td></tr>"#,
                mismatch.path.display(),
                short_a,
                short_b
            ));
            if let Some(diff) = &mismatch.diffoscope_output {
                html.push_str(&format!(
                    r#"<tr class="mismatch"><td colspan="3"><details><summary>diffoscope output</summary><pre>{}</pre></details></td></tr>"#,
                    html_escape(diff)
                ));
            }
            if let Some(zls) = &mismatch.zls_root_output {
                let differing: Vec<&CrateEntry> = zls.crates.iter().filter(|c| !c.hash_match).collect();
                let matching = zls.crates.iter().filter(|c| c.hash_match).count();
                
                let mut rows = String::new();
                for c in &differing {
                    rows.push_str(&format!(
                        "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
                        html_escape(&c.name),
                        html_escape(&c.hash_a),
                        html_escape(&c.hash_b),
                        html_escape(&c.kind),
                        html_escape(&c.linkage),
                    ));
                }
                
                html.push_str(&format!(
                    r#"<tr class="mismatch"><td colspan="3"><details><summary>rustc -Zls=root diff ({} differing, {} matching)</summary><table><thead><tr><th>Crate</th><th>Hash A</th><th>Hash B</th><th>Kind</th><th>Linkage</th></tr></thead><tbody>{}</tbody></table></details></td></tr>"#,
                    differing.len(), matching, rows
                ));

                let mut all_rows = String::new();
                for c in &zls.crates {
                    let status = if c.hash_match { "✔️" } else { "❌" };
                    all_rows.push_str(&format!(
                        "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
                        html_escape(&c.name),
                        html_escape(&c.hash_a),
                        html_escape(&c.hash_b),
                        html_escape(&c.kind),
                        html_escape(&c.linkage),
                        status,
                    ));
                }
                html.push_str(&format!(
                    r#"<tr class="mismatch"><td colspan="3"><details><summary>All crates ({} total)</summary><table><thead><tr><th>Crate</th><th>Hash A</th><th>Hash B</th><th>Kind</th><th>Linkage</th><th>Status</th></tr></thead><tbody>{}</tbody></table></details></td></tr>"#,
                    zls.crates.len(), all_rows
                ));
            }
            if !mismatch.root_cause_crates.is_empty() {
                let items: String = mismatch.root_cause_crates.iter()
                    .map(|c| format!("<li>{}</li>", html_escape(c)))
                    .collect();
                html.push_str(&format!(
                    r#"<tr class="mismatch"><td colspan="3"><details><summary>Root cause candidates ({})</summary><ul>{}</ul></details></td></tr>"#,
                    mismatch.root_cause_crates.len(), items
                ));
            }
                if let Some(dwarf) = &mismatch.dwarf_path_diff {
                let mut rows = String::new();
                for path in &dwarf.only_in_a {
                    rows.push_str(&format!(
                        "<tr><td style='color:red'>only in A</td><td>{}</td></tr>",
                        html_escape(path)
                    ));
                }
                for path in &dwarf.only_in_b {
                    rows.push_str(&format!(
                        "<tr><td style='color:blue'>only in B</td><td>{}</td></tr>",
                        html_escape(path)
                    ));
                }
                html.push_str(&format!(
                    r#"<tr class="mismatch"><td colspan="3"><details><summary>DWARF path diff ({} only in A, {} only in B)</summary><table><thead><tr><th>Side</th><th>Path</th></tr></thead><tbody>{}</tbody></table></details></td></tr>"#,
                    dwarf.only_in_a.len(), dwarf.only_in_b.len(), rows
                ));
            }
        }
        html.push_str(r#"<tr><td colspan="3" style="padding: 1rem; border: none; background: transparent;"></td></tr>"#);
        html.push_str("</tbody></table>");
    }

    fn html_escape(s: &str) -> String {
        s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
    }

    html.push_str(&format!(
        r#"
    </div>
    <div class="section">
        <h2>Ignored Files ({})</h2>"#,
        report.ignored_files.len()
    ));

    if report.ignored_files.is_empty() {
        html.push_str("<p>None ignored this time.</p>");
    } else {
        html.push_str(
            r#"
            <details open>
                <summary>Click to hide/show</summary>
                <table>
                    <thead><tr><th>File</th><th>Matched Pattern</th></tr></thead>
                    <tbody>
        "#,
        );
        for (path, pat) in &report.ignored_files {
            html.push_str(&format!(
                r#"<tr class="ignored"><td>{}</td><td>{}</td></tr>"#,
                path.display(),
                pat
            ));
        }
        html.push_str("</tbody></table></details>");
    }

    html.push_str(&format!(
        r#"
    </div>
    <div class="section">
        <h2>Files Compared ({})</h2>"#,
        report.compared_files.len()
    ));

    if report.compared_files.is_empty() {
        html.push_str("<p>Nothing to compare - maybe all ignored?</p>");
    } else {
        html.push_str(
            r#"
            <details>
                <summary>Expand to see list</summary>
                <ul>
        "#,
        );
        for path in &report.compared_files {
            html.push_str(&format!("<li>{}</li>", path.display()));
        }
        html.push_str("</ul></details>");
    }

    html.push_str(&format!(
        r#"
    </div>
    <footer style="margin-top: 3rem; color: #6c757d; font-size: 0.875rem; text-align: center;">
        Report generated on {timestamp}
    </footer>
</div>
</body>
</html>"#,
        timestamp = timestamp
    ));

    std::fs::write(output_path, html)?;
    info!("Wrote report to {}", output_path.display());
    Ok(())
}

/// Simple hash func - SHA256, copies file content into hasher.
pub fn compute_hash(path: &Path) -> Result<String> {
    let mut f = File::open(path)?;
    let mut hasher = Sha256::new();
    copy(&mut f, &mut hasher)?;
    Ok(hex::encode(hasher.finalize()))
}

#[cfg(windows)]
pub fn compute_normalized_pe_hash(path: &Path) -> Result<PeNormalizationResult> {
    use std::fs;

    let mut bytes = fs::read(path)?;
    let mut notes = Vec::new();

    if bytes.len() < 64 {
        return Ok(PeNormalizationResult {
            normalized_hash: "UNSUPPORTED".into(),
            notes: vec!["file too small".into()],
        });
    }

    // --- 1. Locate the PE signature dynamically ---
    let pe_offset = u32::from_le_bytes([
        bytes[0x3c], bytes[0x3d], bytes[0x3e], bytes[0x3f],
    ]) as usize;
    if pe_offset + 4 > bytes.len() || &bytes[pe_offset..pe_offset + 4] != b"PE\0\0" {
        return Ok(PeNormalizationResult {
            normalized_hash: "NOT_PE".into(),
            notes: vec!["no PE signature".into()],
        });
    }

    // --- 2. Read the original PE timestamp ---
    let coff_time_offset = pe_offset + 4 + 4; // skip Machine + NumberOfSections (4 bytes)
    let ts = if coff_time_offset + 4 <= bytes.len() {
        u32::from_le_bytes([
            bytes[coff_time_offset],
            bytes[coff_time_offset + 1],
            bytes[coff_time_offset + 2],
            bytes[coff_time_offset + 3],
        ])
    } else {
        0
    };

    if ts != 0 {
        let ts_bytes = ts.to_le_bytes();
        // zero *every* occurrence of this timestamp anywhere in the file
        let mut offset = 0;
        while offset + 4 <= bytes.len() {
            if bytes[offset..offset + 4] == ts_bytes {
                bytes[offset..offset + 4].fill(0);
            }
            offset += 1;
        }
        notes.push(format!("zeroed PE timestamp {:#x} globally", ts));
    }

    // --- 3. Zero all PDB GUIDs (RSDS signatures) ---
    let rsds = [0x52, 0x53, 0x44, 0x53]; // "RSDS"
    let mut offset = 0;
    while offset + 24 <= bytes.len() {
        if bytes[offset..offset + 4] == rsds {
            bytes[offset + 4..offset + 20].fill(0); // 16-byte GUID
            notes.push(format!("zeroed PDB GUID at offset 0x{:X}", offset));
            offset += 20;
        } else {
            offset += 1;
        }
    }

    let mut hasher = Sha256::new();
    hasher.update(&bytes);

    Ok(PeNormalizationResult {
        normalized_hash: hex::encode(hasher.finalize()),
        notes,
    })
}