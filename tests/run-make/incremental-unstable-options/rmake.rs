//@ needs-target-std

// Regression test for #166701.
// Enabling unstable options changes the registered internal lint store.
// Cached lint levels must not be reused across this change.

use run_make_support::rustc;

fn main() {
    for initial in [false, true] {
        let cache = format!("incremental-{initial}");

        for enabled in [initial, !initial, initial] {
            let mut cmd = rustc();
            cmd.input("lib.rs")
                .crate_type("rlib")
                .arg("-Wrustc::potential_query_instability")
                .arg(format!("-Cincremental={cache}"))
                .output("libexample.rlib");

            if enabled {
                cmd.arg("-Zunstable-options");
            }

            cmd.run();
        }
    }
}
