//! `render-template MODULE_NAME TEMPLATE_DIR OUT_DIR` — see the crate docs.
//!
//! This runs at init time, in the toolchain image `rust-sdk.dang` builds it in,
//! and has nothing to do with the module runtime. It is a goish binary all the
//! same: `no_std`, statically linked, no libc.

#![no_std]
#![no_main]

use goish::{bytes, fmt, len, nil, os, string};
use render_template::run;

/// Write a message to stderr and exit non-zero.
fn fail(msg: string) -> ! {
    let stderr = os::Stderr();
    let _ = stderr.Write(bytes(string("render-template: ") + msg + "\n"));
    os::Exit(1)
}

#[goish::main]
fn main() {
    // argv[0] is the program name.
    let args = os::Args();
    let err = run(args.slice(1, len(&args)));
    if err != nil {
        fail(fmt::Sprintf!("%v", err));
    }
}
