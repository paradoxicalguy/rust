// Changing compiler options must not reuse incompatible incremental results.
// See https://github.com/rust-lang/rust/issues/84232.

//@ needs-target-std
//@ ignore-backends: gcc cranelift

use std::fs;

use run_make_support::rustc;

fn main() {
    codegen_source_order();
    no_leak_check();
    future_incompat_test();
    build_sdylib_interface();
    trim_diagnostic_paths();
}

fn codegen_source_order() {
    for initial in ["no", "yes"] {
        let cache = format!("order-{initial}");
        let other = if initial == "yes" { "no" } else { "yes" };

        for state in [initial, other, initial] {
            rustc()
                .input("order.rs")
                .edition("2021")
                .crate_type("rlib")
                .codegen_units(1)
                .arg("-Copt-level=0")
                .arg(format!("-Cincremental={cache}"))
                .arg(format!("-Zcodegen-source-order={state}"))
                .emit("llvm-ir")
                .output("order.ll")
                .run();

            let ir = fs::read_to_string("order.ll").unwrap();
            let apple = ir.find("@apple(").unwrap();
            let mango = ir.find("@mango(").unwrap();
            let zebra = ir.find("@zebra(").unwrap();

            if state == "yes" {
                assert!(zebra < mango && mango < apple, "unexpected function order for {state}");
            } else {
                assert!(apple < mango && mango < zebra, "unexpected function order for {state}");
            }
        }
    }
}

fn no_leak_check() {
    for enabled in [true, false, true] {
        let mut cmd = rustc();
        cmd.input("leak.rs")
            .crate_type("rlib")
            .arg("-Cincremental=leak-cache")
            .emit("metadata")
            .output("libleak.rmeta");

        if enabled {
            cmd.arg("-Zno-leak-check").run();
        } else {
            cmd.run_fail().assert_stderr_contains("E0308");
        }
    }
}

fn future_incompat_test() {
    for initial in ["no", "yes"] {
        let cache = format!("lint-{initial}");
        let other = if initial == "yes" { "no" } else { "yes" };

        for state in [initial, other, initial] {
            let result = rustc()
                .input("lint.rs")
                .crate_type("rlib")
                .arg("--error-format=json")
                .arg("--json=future-incompat")
                .arg(format!("-Cincremental={cache}"))
                .arg(format!("-Zfuture-incompat-test={state}"))
                .output("liblint.rlib")
                .run();

            if state == "yes" {
                result.assert_stderr_contains("\"future_incompat_report\":[{");
            } else {
                result.assert_stderr_not_contains("\"future_incompat_report\":[{");
            }
        }
    }
}

fn build_sdylib_interface() {
    for initial in ["no", "yes"] {
        let cache = format!("sdylib-{initial}");
        let other = if initial == "yes" { "no" } else { "yes" };

        for state in [initial, other, initial] {
            rustc()
                .input("interface.rs")
                .crate_type("rlib")
                .arg(format!("-Cincremental={cache}"))
                .arg(format!("-Zbuild-sdylib-interface={state}"))
                .emit("mir")
                .output("interface.mir")
                .run();

            let mir = fs::read_to_string("interface.mir").unwrap();

            if state == "yes" {
                assert!(mir.contains("unreachable;"));
                assert!(!mir.contains("wrapping_add"));
            } else {
                assert!(mir.contains("wrapping_add"));
                assert!(!mir.contains("unreachable;"));
            }
        }
    }
}

fn trim_diagnostic_paths() {
    // Cached diagnostics must respect changes to path trimming.
    for initial in ["yes", "no"] {
        let other = if initial == "yes" { "no" } else { "yes" };
        let cache = format!("trim-diagnostic-paths-{initial}");

        for state in [initial, other, initial] {
            let output = rustc()
                .input("trim.rs")
                .crate_type("rlib")
                .arg("-Wunused-must-use")
                .arg(format!("-Cincremental={cache}"))
                .arg(format!("-Ztrim-diagnostic-paths={state}"))
                .output("libtrim.rlib")
                .run();

            if state == "yes" {
                output.assert_stderr_contains("unused `Important` that must be used");
                output.assert_stderr_not_contains(
                    "unused `outer::inner::Important` that must be used",
                );
            } else {
                output.assert_stderr_contains("unused `outer::inner::Important` that must be used");
                output.assert_stderr_not_contains("unused `Important` that must be used");
            }
        }
    }
}
