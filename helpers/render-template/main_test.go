package main

import (
	"os"
	"path/filepath"
	"testing"
)

// TestNameConversionsMatchDang pins the expected output of the name conversions
// against the behaviour of toRustCrateName in runtime/main.dang. The runtime
// computes the binary's filename from the Dagger module name at call time; this
// helper writes the cargo [package] name at init time. A divergence produces a
// module that builds but whose entrypoint points at a path that does not exist,
// so the acronym and digit cases below are the ones worth guarding — a
// general-purpose case library gets them wrong (strcase.ToCamel("HTTPServer")
// == "Httpserver").
func TestNameConversionsMatchDang(t *testing.T) {
	for _, tc := range []struct {
		name      string
		crateName string
		typeName  string
	}{
		{"my-module", "my_module", "MyModule"},
		{"my_module", "my_module", "MyModule"},
		{"myModule", "my_module", "MyModule"},
		{"MyModule", "my_module", "MyModule"},
		{"mymodule", "mymodule", "Mymodule"},
		{"HTTPServer", "http_server", "HttpServer"},
		{"MyHTTPServer", "my_http_server", "MyHttpServer"},
		{"http-server", "http_server", "HttpServer"},
		{"foo2bar", "foo2bar", "Foo2bar"},
		{"Foo-Bar_baz", "foo_bar_baz", "FooBarBaz"},
		{"--leading-and-trailing--", "leading_and_trailing", "LeadingAndTrailing"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			if got := rustCrateName(tc.name); got != tc.crateName {
				t.Errorf("rustCrateName(%q) = %q, want %q", tc.name, got, tc.crateName)
			}
			if got := rustTypeName(tc.name); got != tc.typeName {
				t.Errorf("rustTypeName(%q) = %q, want %q", tc.name, got, tc.typeName)
			}
		})
	}
}

func TestRunRendersTemplate(t *testing.T) {
	tmplDir := t.TempDir()
	outDir := filepath.Join(t.TempDir(), "out")

	write(t, filepath.Join(tmplDir, "Cargo.toml.tmpl"), "name = \"{{ .ModuleCrate }}\" # {{ .ModuleName }}\n")
	write(t, filepath.Join(tmplDir, "src", "{{.ModuleCrate}}.rs.tmpl"), "struct {{ .ModuleType }};\n")
	write(t, filepath.Join(tmplDir, ".cargo", "config.toml"), "[build]\ntarget = \"x86_64-unknown-linux-gnu\"\n")

	if err := run([]string{"my-module", tmplDir, outDir}); err != nil {
		t.Fatalf("run: %v", err)
	}

	// .tmpl suffix stripped and contents rendered
	assertFile(t, filepath.Join(outDir, "Cargo.toml"), "name = \"my_module\" # my-module\n")
	// templated path segment resolved
	assertFile(t, filepath.Join(outDir, "src", "my_module.rs"), "struct MyModule;\n")
	// non-.tmpl copied verbatim, keeping its name
	assertFile(t, filepath.Join(outDir, ".cargo", "config.toml"), "[build]\ntarget = \"x86_64-unknown-linux-gnu\"\n")
}

func TestRunRejectsNameWithoutAlphanumerics(t *testing.T) {
	if err := run([]string{"---", t.TempDir(), filepath.Join(t.TempDir(), "out")}); err == nil {
		t.Fatal("expected an error for a module name with no alphanumeric characters")
	}
}

func TestRunRejectsNameStartingWithDigit(t *testing.T) {
	if err := run([]string{"2fast", t.TempDir(), filepath.Join(t.TempDir(), "out")}); err == nil {
		t.Fatal("expected an error for a module name yielding a crate name that starts with a digit")
	}
}

func write(t *testing.T, path, contents string) {
	t.Helper()
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path, []byte(contents), 0o644); err != nil {
		t.Fatal(err)
	}
}

func assertFile(t *testing.T, path, want string) {
	t.Helper()
	got, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("read %s: %v", path, err)
	}
	if string(got) != want {
		t.Errorf("%s = %q, want %q", path, got, want)
	}
}
