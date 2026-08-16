//! Test suite for `render-template`.
//!
//! ```sh
//! cd helpers/render-template && cargo test
//! ```
//!
//! This is a `harness = false` target: libtest is `std`, and its `panic_impl`
//! collides with goish's, so there are no `#[test]` functions to collect. goish
//! ships Go's `testing` package instead, so the tests are ordinary functions
//! assembled into a list and handed to `testing::Main` — the same shape
//! `go test` generates — and cargo reads the exit status. Add a test to the
//! list in `main` or it never runs.

#![no_std]
#![no_main]
#![allow(non_snake_case)]

use goish::path::filepath;
use goish::{append, bytes, fmt, len, make, nil, os, regexp, slice, string, strings, testing};
use render_template::{is_rust_keyword, render, run, rust_crate_name, rust_type_name, TemplateData};

/// `runtime/main.dang`, resolved from this crate's location rather than the
/// working directory: the test binary is run from wherever cargo was invoked.
const DANG_RUNTIME: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../runtime/main.dang");

// ─── Name conversions ────────────────────────────────────────────────

/// Pins the expected output of the name conversions against the behaviour of
/// `toRustCrateName` in `runtime/main.dang`. The runtime computes the binary's
/// filename from the Dagger module name at call time; this helper writes the
/// cargo `[package]` name at init time. A divergence produces a module that
/// builds but whose entrypoint points at a path that does not exist, so the
/// acronym and digit cases below are the ones worth guarding — a
/// general-purpose case library gets them wrong (`strcase.ToCamel("HTTPServer")
/// == "Httpserver"`).
fn TestNameConversionsMatchDang(t: &mut testing::T) {
    const CASES: &[(&str, &str, &str)] = &[
        ("my-module", "my_module", "MyModule"),
        ("my_module", "my_module", "MyModule"),
        ("myModule", "my_module", "MyModule"),
        ("MyModule", "my_module", "MyModule"),
        ("mymodule", "mymodule", "Mymodule"),
        ("HTTPServer", "http_server", "HttpServer"),
        ("MyHTTPServer", "my_http_server", "MyHttpServer"),
        ("http-server", "http_server", "HttpServer"),
        ("foo2bar", "foo2bar", "Foo2bar"),
        ("Foo-Bar_baz", "foo_bar_baz", "FooBarBaz"),
        ("--leading-and-trailing--", "leading_and_trailing", "LeadingAndTrailing"),
    ];

    for &(name, crate_name, type_name) in CASES {
        t.Run(string(name), move |t| {
            let got = rust_crate_name(string(name));
            if got != string(crate_name) {
                t.Error(fmt::Sprintf!(
                    "rust_crate_name(%q) = %q, want %q",
                    name,
                    got,
                    crate_name
                ));
            }
            let got = rust_type_name(string(name));
            if got != string(type_name) {
                t.Error(fmt::Sprintf!(
                    "rust_type_name(%q) = %q, want %q",
                    name,
                    got,
                    type_name
                ));
            }
        });
    }
}

/// The transformations `toRustCrateName` applies, in order, extracted straight
/// out of `runtime/main.dang` as (pattern, replacement) pairs.
///
/// The dang function is three `replaceMatches` calls followed by `trim("_")`
/// and `toLower`, and `replaceMatches` is Go's `regexp.ReplaceAllString` — so
/// replaying the extracted pairs reproduces the dang result exactly rather than
/// approximating it.
fn dangCrateNameRules(t: &mut testing::T) -> (slice<string>, slice<string>) {
    let (src, err) = os::ReadFile(string(DANG_RUNTIME));
    if err != nil {
        t.Fatal(fmt::Sprintf!("read %s: %v", DANG_RUNTIME, err));
    }
    let src = string(src);

    let start = strings::Index(src.clone(), "let toRustCrateName(");
    if start < 0 {
        t.Fatal("could not find toRustCrateName in runtime/main.dang");
    }
    let tail = src.slice(start, src.Len());
    let end = strings::Index(tail.clone(), "\n}");
    if end < 0 {
        t.Fatal("could not find the end of toRustCrateName in runtime/main.dang");
    }
    let body = tail.slice(0, end + 2);

    let re = regexp::MustCompile("\\.replaceMatches\\(`([^`]*)`, with: \"([^\"]*)\"\\)");
    let matches = re.FindAllStringSubmatch(body.clone(), -1);

    let mut patterns = make!([]string, 0, 3);
    let mut replacements = make!([]string, 0, 3);
    let mut i = 0;
    while i < len(&matches) as usize {
        let row = matches[i].clone();
        patterns = append!(patterns, row[1].clone());
        replacements = append!(replacements, row[2].clone());
        i += 1;
    }

    // Guard the shape the replay assumes: if the dang function grows a step,
    // this test must learn about it rather than quietly checking a stale recipe.
    if len(&patterns) != 3 {
        t.Fatal(fmt::Sprintf!(
            "expected 3 replaceMatches calls in toRustCrateName, found %d",
            len(&patterns)
        ));
    }
    if !strings::Contains(body.clone(), "  .trim(\"_\")") || !strings::Contains(body, ".toLower") {
        t.Fatal("toRustCrateName no longer ends in .trim(\"_\").toLower");
    }

    (patterns, replacements)
}

/// Expand `${name}` / `$name` references in a replacement template against one
/// match's groups, exactly as Go's `Regexp.Expand` does.
///
/// Reproducing Go's *misfeature* here is the point: `$1_` names the group
/// called `1_`, not group 1 followed by an underscore, and an unknown name
/// expands to the empty string. That is the bug `toRustCrateName` shipped with,
/// and the replay only catches its return if the replay gets it wrong the same
/// way Go does.
fn expand(repl: string, groups: &slice<string>) -> string {
    let src = repl.as_bytes();
    let mut out = strings::Builder::new();

    let mut i = 0;
    while i < src.len() {
        if src[i] != b'$' {
            let _ = out.WriteByte(src[i]);
            i += 1;
            continue;
        }
        if i + 1 < src.len() && src[i + 1] == b'$' {
            let _ = out.WriteByte(b'$');
            i += 2;
            continue;
        }

        let braced = i + 1 < src.len() && src[i + 1] == b'{';
        let mut j = if braced { i + 2 } else { i + 1 };
        let name_start = j;
        while j < src.len() && (src[j].is_ascii_alphanumeric() || src[j] == b'_') {
            j += 1;
        }
        let name = &src[name_start..j];
        if braced {
            if j >= src.len() || src[j] != b'}' {
                // Malformed: Go leaves the whole thing alone.
                let _ = out.WriteByte(b'$');
                i += 1;
                continue;
            }
            j += 1;
        }
        if name.is_empty() {
            let _ = out.WriteByte(b'$');
            i += 1;
            continue;
        }

        // An all-digit name is a group number; anything else is a group *name*,
        // and these patterns have none. Either way an out-of-range reference
        // expands to nothing.
        if name.iter().all(|c| c.is_ascii_digit()) {
            let mut n: usize = 0;
            for c in name {
                n = n * 10 + (c - b'0') as usize;
            }
            if n < len(groups) as usize {
                let _ = out.WriteString(groups[n].clone());
            }
        }
        i = j;
    }

    out.String()
}

/// Go's `Regexp.ReplaceAllString`, including the `${n}` expansion goish's
/// `ReplaceAllString` leaves out of its v1 subset.
fn replaceAllString(re: &regexp::Regexp, src: string, repl: string) -> string {
    let spans = re.FindAllStringIndex(src.clone(), -1);
    let groups = re.FindAllStringSubmatch(src.clone(), -1);
    let mut out = strings::Builder::new();

    let mut last: i64 = 0;
    let mut i = 0;
    while i < len(&spans) as usize {
        let span = spans[i].clone();
        let _ = out.WriteString(src.slice(last, span[0]));
        let _ = out.WriteString(expand(repl.clone(), &groups[i]));
        last = span[1];
        i += 1;
    }
    let _ = out.WriteString(src.slice(last, src.Len()));

    out.String()
}

/// Replays `runtime/main.dang`'s `toRustCrateName` and checks it against
/// `rust_crate_name` for every case the table above covers.
///
/// `TestNameConversionsMatchDang` only pins the Rust side, so it cannot see the
/// dang side drift. It didn't: `toRustCrateName` shipped with `"$1_$2"`
/// replacements, which Go's Expand reads as a reference to a group *named*
/// `1_` — silently turning `"HTTPServer"` into `"server"` and breaking every
/// camel-cased module, while the helper-only test stayed green.
///
/// The list runs well past the table above: the dang recipe is the *oracle* the
/// hand-written scan in `rust_crate_name` is checked against, so every name
/// here is a chance for the two to disagree — acronyms at either end, digits
/// against case boundaries, single letters, punctuation runs, and bytes outside
/// ASCII.
fn TestDangCrateNameMatchesRust(t: &mut testing::T) {
    const NAMES: &[&str] = &[
        "my-module",
        "my_module",
        "myModule",
        "MyModule",
        "mymodule",
        "HTTPServer",
        "MyHTTPServer",
        "http-server",
        "foo2bar",
        "Foo-Bar_baz",
        "--leading-and-trailing--",
        // Acronym runs, at both ends and back to back.
        "HTTP",
        "HTTPServerAPI",
        "APIHTTPServer",
        "ABCDef",
        "ABcDEf",
        "aXYZb",
        "OpenAPIV3Schema",
        // Digits either side of a case boundary.
        "v2Module",
        "Module2X",
        "x2Y",
        "2fast2furious",
        "foo2Bar",
        // Single letters and minimal inputs.
        "a",
        "A",
        "aB",
        "Ab",
        "",
        "-",
        // Punctuation runs, mixed separators, and bytes outside ASCII.
        "a__b",
        "a.b c/d",
        "___",
        "Über-Modul",
        "módulo",
        "日本語モジュール",
    ];

    let (patterns, replacements) = dangCrateNameRules(t);

    for &name in NAMES {
        let mut got = string(name);
        let mut i = 0;
        while i < len(&patterns) as usize {
            let re = regexp::MustCompile(patterns[i].clone());
            got = replaceAllString(&re, got, replacements[i].clone());
            i += 1;
        }
        let got = strings::ToLower(strings::Trim(got, "_"));

        let want = rust_crate_name(string(name));
        if got != want {
            t.Error(fmt::Sprintf!(
                "toRustCrateName(%q) = %q, but rust_crate_name(%q) = %q — the runtime would look for a binary cargo never built",
                name, got, name, want
            ));
        }
    }
}

// ─── Rendering ───────────────────────────────────────────────────────

fn TestRunRendersTemplate(t: &mut testing::T) {
    let tmplDir = tempDir(t);
    let outDir = join(tempDir(t), string("out"));

    write(
        t,
        join(tmplDir.clone(), string("Cargo.toml.tmpl")),
        "name = \"{{ .ModuleCrate }}\" # {{ .ModuleName }}\n",
    );
    write(
        t,
        join(tmplDir.clone(), string("src/{{.ModuleCrate}}.rs.tmpl")),
        "struct {{ .ModuleType }};\n",
    );
    write(
        t,
        join(tmplDir.clone(), string(".cargo/config.toml")),
        "[build]\ntarget = \"x86_64-unknown-linux-gnu\"\n",
    );

    let err = run(args(string("my-module"), tmplDir, outDir.clone()));
    if err != nil {
        t.Fatal(fmt::Sprintf!("run: %v", err));
    }

    // .tmpl suffix stripped and contents rendered.
    assertFile(
        t,
        join(outDir.clone(), string("Cargo.toml")),
        string("name = \"my_module\" # my-module\n"),
    );
    // Templated path segment resolved.
    assertFile(
        t,
        join(outDir.clone(), string("src/my_module.rs")),
        string("struct MyModule;\n"),
    );
    // Non-.tmpl copied verbatim, keeping its name.
    assertFile(
        t,
        join(outDir, string(".cargo/config.toml")),
        string("[build]\ntarget = \"x86_64-unknown-linux-gnu\"\n"),
    );
}

fn TestRunRejectsNameWithoutAlphanumerics(t: &mut testing::T) {
    let err = run(args(string("---"), tempDir(t), join(tempDir(t), string("out"))));
    if err == nil {
        t.Fatal("expected an error for a module name with no alphanumeric characters");
    }
}

fn TestRunRejectsNameStartingWithDigit(t: &mut testing::T) {
    let err = run(args(string("2fast"), tempDir(t), join(tempDir(t), string("out"))));
    if err == nil {
        t.Fatal("expected an error for a module name yielding a crate name that starts with a digit");
    }
}

/// Covers names whose Rust type name is reserved. `pub struct Self;` is a parse
/// error ("expected identifier, found keyword `Self`"), so the scaffold has to
/// refuse rather than emit a project that cannot build. Every spelling that
/// lowercases to "self" lands on the same type name.
fn TestRunRejectsReservedTypeName(t: &mut testing::T) {
    const NAMES: &[&str] = &["self", "Self", "SELF", "-self-", "_self_"];

    for &name in NAMES {
        t.Run(string(name), move |t| {
            let got = rust_type_name(string(name));
            if got != string("Self") {
                t.Fatal(fmt::Sprintf!(
                    "precondition: rust_type_name(%q) = %q, want %q",
                    name,
                    got,
                    "Self"
                ));
            }
            let err = run(args(string(name), tempDir(t), join(tempDir(t), string("out"))));
            if err == nil {
                t.Error(fmt::Sprintf!(
                    "expected an error for module name %q, which yields the type name Self",
                    name
                ));
            }
        });
    }
}

/// Pins the other half of the rule. These names produce a crate name that is a
/// Rust keyword, but the crate name is only ever a cargo package name, a bin
/// target name and a filename — never an identifier — so cargo builds them and
/// the scaffold must not reject them. Verified by building a bin crate named
/// `crate`, `self`, `type` and `move`.
fn TestRunAllowsKeywordCrateName(t: &mut testing::T) {
    const CASES: &[(&str, &str, &str)] = &[
        ("crate", "crate", "Crate"),
        ("type", "type", "Type"),
        ("move", "move", "Move"),
        ("box", "box", "Box"),
    ];

    for &(name, crateName, typeName) in CASES {
        t.Run(string(name), move |t| {
            let got = rust_crate_name(string(name));
            if got != string(crateName) {
                t.Fatal(fmt::Sprintf!(
                    "precondition: rust_crate_name(%q) = %q, want %q",
                    name,
                    got,
                    crateName
                ));
            }
            if !is_rust_keyword(&string(crateName)) {
                t.Fatal(fmt::Sprintf!(
                    "precondition: %q is expected to be a Rust keyword",
                    crateName
                ));
            }

            let tmplDir = tempDir(t);
            let outDir = join(tempDir(t), string("out"));
            write(
                t,
                join(tmplDir.clone(), string("Cargo.toml.tmpl")),
                "name = \"{{ .ModuleCrate }}\"\n",
            );
            write(
                t,
                join(tmplDir.clone(), string("src/main.rs.tmpl")),
                "pub struct {{ .ModuleType }};\n",
            );

            let err = run(args(string(name), tmplDir, outDir.clone()));
            if err != nil {
                t.Fatal(fmt::Sprintf!("run(%q): %v", name, err));
            }
            assertFile(
                t,
                join(outDir.clone(), string("Cargo.toml")),
                string("name = \"") + crateName + "\"\n",
            );
            assertFile(
                t,
                join(outDir, string("src/main.rs")),
                string("pub struct ") + typeName + ";\n",
            );
        });
    }
}

/// The renderer is a subset of Go's text/template, so the actions it does not
/// implement have to fail loudly. Go would have rendered `{{ .Nope }}` as
/// `<no value>` and scaffolded a module around it.
fn TestRenderRejectsUnsupportedActions(t: &mut testing::T) {
    const CASES: &[(&str, &str)] = &[
        ("unknown field", "{{ .Nope }}"),
        ("nested field", "{{ .Module.Name }}"),
        ("pipeline", "{{ .ModuleName | printf \"%s\" }}"),
        ("control flow", "{{ if .ModuleName }}x{{ end }}"),
        ("trim marker", "{{- .ModuleName }}"),
        ("comment", "{{/* nothing */}}"),
        ("unterminated", "name = {{ .ModuleName"),
    ];

    for &(label, text) in CASES {
        t.Run(string(label), move |t| {
            let (data, err) = TemplateData::new(string("my-module"));
            if err != nil {
                t.Fatal(fmt::Sprintf!("TemplateData::new: %v", err));
            }
            let (got, err) = render(string(text), &data, string("template.tmpl"));
            if err == nil {
                t.Error(fmt::Sprintf!("render(%q) = %q, want an error", text, got));
            }
        });
    }
}

/// The actions the templates in this repository actually use.
fn TestRenderSubstitutesFields(t: &mut testing::T) {
    const CASES: &[(&str, &str)] = &[
        ("{{ .ModuleName }}", "my-module"),
        ("{{.ModuleCrate}}", "my_module"),
        ("{{\t.ModuleType\n}}", "MyModule"),
        ("no actions at all", "no actions at all"),
        (
            "{{ .ModuleType }} in {{ .ModuleCrate }}!",
            "MyModule in my_module!",
        ),
    ];

    let (data, err) = TemplateData::new(string("my-module"));
    if err != nil {
        t.Fatal(fmt::Sprintf!("TemplateData::new: %v", err));
    }

    for &(text, want) in CASES {
        let (got, err) = render(string(text), &data, string("template.tmpl"));
        if err != nil {
            t.Error(fmt::Sprintf!("render(%q): %v", text, err));
        } else if got != string(want) {
            t.Error(fmt::Sprintf!("render(%q) = %q, want %q", text, got, want));
        }
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────

/// A temporary directory removed when the test ends — `t.TempDir()`.
fn tempDir(t: &testing::T) -> string {
    let (dir, err) = os::MkdirTemp("", "render-template-test-*");
    if err != nil {
        t.Fatal(fmt::Sprintf!("create temp dir: %v", err));
    }
    let cleanup = dir.clone();
    t.Cleanup(move || {
        let _ = os::RemoveAll(cleanup.clone());
    });
    dir
}

fn join(a: string, b: string) -> string {
    let elems = make!([]string, 0, 2);
    let elems = append!(elems, a, b);
    filepath::Join(elems)
}

fn args(moduleName: string, templateDir: string, outDir: string) -> slice<string> {
    let argv = make!([]string, 0, 3);
    append!(argv, moduleName, templateDir, outDir)
}

fn write(t: &testing::T, path: string, contents: &'static str) {
    let err = os::MkdirAll(filepath::Dir(path.clone()), 0o755);
    if err != nil {
        t.Fatal(fmt::Sprintf!("create %s: %v", filepath::Dir(path), err));
    }
    let err = os::WriteFile(path.clone(), bytes(string(contents)), 0o644);
    if err != nil {
        t.Fatal(fmt::Sprintf!("write %s: %v", path, err));
    }
}

fn assertFile(t: &testing::T, path: string, want: string) {
    let (got, err) = os::ReadFile(path.clone());
    if err != nil {
        t.Fatal(fmt::Sprintf!("read %s: %v", path, err));
    }
    let got = string(got);
    if got != want {
        t.Error(fmt::Sprintf!("%s = %q, want %q", path, got, want));
    }
}

#[goish::main]
fn main() {
    let tests: &[(&str, testing::TestFn)] = &[
        ("TestNameConversionsMatchDang", TestNameConversionsMatchDang),
        ("TestDangCrateNameMatchesRust", TestDangCrateNameMatchesRust),
        ("TestRunRendersTemplate", TestRunRendersTemplate),
        (
            "TestRunRejectsNameWithoutAlphanumerics",
            TestRunRejectsNameWithoutAlphanumerics,
        ),
        (
            "TestRunRejectsNameStartingWithDigit",
            TestRunRejectsNameStartingWithDigit,
        ),
        ("TestRunRejectsReservedTypeName", TestRunRejectsReservedTypeName),
        ("TestRunAllowsKeywordCrateName", TestRunAllowsKeywordCrateName),
        (
            "TestRenderRejectsUnsupportedActions",
            TestRenderRejectsUnsupportedActions,
        ),
        ("TestRenderSubstitutesFields", TestRenderSubstitutesFields),
    ];
    os::Exit(testing::Main(tests));
}
