// Ensure that rustdoc respects documentation-scoped path remapping when
// generating source pages and links, including for files outside the crate
// source root.
//
// See https://github.com/rust-lang/rust/issues/18370.

//@ needs-target-std

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use run_make_support::{cwd, rfs, rustdoc};

fn main() {
    let base = cwd();

    for dir in ["a", "longer-b"] {
        let root = base.join(dir);
        rfs::create_dir_all(root.join("src"));
        rfs::create_dir_all(root.join("generated"));

        rfs::write(
            root.join("src/lib.rs"),
            "pub fn ordinary() {}\n\
             include!(concat!(env!(\"OUT_DIR\"), \"/generated.rs\"));\n",
        );
        rfs::write(root.join("generated/generated.rs"), "pub fn generated() {}\n");

        std::env::set_current_dir(&root).unwrap();

        for absolute in [false, true] {
            let input = if absolute {
                root.join("src/lib.rs")
            } else {
                PathBuf::from("src/lib.rs")
            };
            let output = if absolute { "doc-absolute" } else { "doc-relative" };

            rustdoc()
                .edition("2021")
                .crate_name("repro_doc")
                .input(&input)
                .out_dir(output)
                .env("OUT_DIR", root.join("generated"))
                .arg("-Zunstable-options")
                .arg("--remap-path-scope=documentation")
                .arg(format!("--remap-path-prefix={}=/repro", root.display()))
                .run();

            let output = Path::new(output);
            assert!(output.join("src/repro_doc/lib.rs.html").is_file());
            assert!(output.join("src/repro_doc/repro/generated/generated.rs.html").is_file());

            let generated = rfs::read_to_string(output.join("repro_doc/fn.generated.html"));
            assert!(generated.contains(
                "../src/repro_doc/repro/generated/generated.rs.html#1"
            ));

            let ordinary = rfs::read_to_string(output.join("repro_doc/fn.ordinary.html"));
            assert!(ordinary.contains("../src/repro_doc/lib.rs.html#1"));

            let sidebar = rfs::read_to_string(output.join("src-files.js"));
            assert!(sidebar.contains("\"lib.rs\""));

            // Remapping only debuginfo must not remap documentation paths.
            rustdoc()
                .edition("2021")
                .crate_name("repro_doc")
                .input(&input)
                .out_dir(if absolute { "debug-absolute" } else { "debug-relative" })
                .env("OUT_DIR", root.join("generated"))
                .arg("-Zunstable-options")
                .arg("--remap-path-scope=debuginfo")
                .arg(format!("--remap-path-prefix={}=/repro", root.display()))
                .run();

            let debug_output =
                Path::new(if absolute { "debug-absolute" } else { "debug-relative" });

            assert!(debug_output.join("src/repro_doc/lib.rs.html").is_file());
            assert!(
                !debug_output.join("src/repro_doc/repro/generated/generated.rs.html").exists()
            );
        }

        std::env::set_current_dir(&base).unwrap();
    }

    for output in ["doc-relative", "doc-absolute"] {
        let first = files(&base.join("a").join(output));
        let second = files(&base.join("longer-b").join(output));

        assert_eq!(first.keys().collect::<Vec<_>>(), second.keys().collect::<Vec<_>>());

        for (path, contents) in first {
            assert!(contents == second[&path], "different contents in {}", path.display());
        }
    }
}

fn files(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn visit(root: &Path, dir: &Path, result: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                visit(root, &path, result);
            } else {
                result.insert(path.strip_prefix(root).unwrap().to_owned(), rfs::read(&path));
            }
        }
    }

    let mut result = BTreeMap::new();
    visit(root, root, &mut result);
    result
}
