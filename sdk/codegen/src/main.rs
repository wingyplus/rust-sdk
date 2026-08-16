//! `codegen --introspection <schema.json> --outdir <dir>` — see the crate docs.
//!
//! Reads the engine's GraphQL introspection schema and writes the bindings
//! module to `--outdir`, which `dagger generate` vendors into each module at
//! `dagger/src/gen/`.

#![no_std]
#![no_main]

use codegen::{generate, OUTPUT_FILE};
use goish::{bytes, fmt, len, nil, os, string};

/// Write a message to stderr and exit non-zero.
fn fail(msg: string) -> ! {
    let stderr = os::Stderr();
    let _ = stderr.Write(bytes(string("codegen: ") + msg + "\n"));
    os::Exit(1)
}

/// Parse `--introspection` and `--outdir` out of argv.
///
/// Both are required; anything else is an error rather than a silent default,
/// so a typo in mod.dang surfaces here instead of producing empty bindings.
fn parse_args() -> (string, string) {
    let args = os::Args();
    let argc = len(&args);

    let mut introspection = string("");
    let mut outdir = string("");

    // argv[0] is the program name.
    let mut i: goish::int = 1;
    while i < argc {
        let flag = args[i as usize].clone();
        if i + 1 >= argc {
            fail(flag + " requires a value");
        }
        let value = args[(i + 1) as usize].clone();

        if flag == "--introspection" {
            introspection = value;
        } else if flag == "--outdir" {
            outdir = value;
        } else {
            fail(string("unknown argument: ") + flag);
        }
        i += 2;
    }

    if introspection.Len() == 0 {
        fail(string("missing --introspection"));
    }
    if outdir.Len() == 0 {
        fail(string("missing --outdir"));
    }

    (introspection, outdir)
}

#[goish::main]
fn main() {
    let (introspection, outdir) = parse_args();

    let (schema, err) = os::ReadFile(introspection.clone());
    if err != nil {
        fail(string("read ") + introspection + ": " + fmt::Sprintf!("%v", err));
    }
    if len(&schema) == 0 {
        fail(introspection + " is empty");
    }

    let (source, err) = generate(&schema);
    if err != nil {
        fail(string("generating from ") + introspection + ": " + fmt::Sprintf!("%v", err));
    }

    let err = os::MkdirAll(outdir.clone(), 0o755);
    if err != nil {
        fail(string("create ") + outdir + ": " + fmt::Sprintf!("%v", err));
    }

    let out = outdir + "/" + OUTPUT_FILE;
    let err = os::WriteFile(out.clone(), bytes(source), 0o644);
    if err != nil {
        fail(string("write ") + out + ": " + fmt::Sprintf!("%v", err));
    }
}
