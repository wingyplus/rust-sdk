package main

import (
	"bytes"
	"fmt"
	"os"
	"path/filepath"
	"regexp"
	"strings"
	"text/template"
	"unicode"
)

// render-template MODULE_NAME TEMPLATE_DIR OUT_DIR
//
// Walks TEMPLATE_DIR and writes the result to OUT_DIR. Files ending in ".tmpl"
// are rendered as Go text/templates and lose the suffix; everything else is
// copied verbatim. Path segments containing "{{" are rendered too, so a
// template can name a file after the module (e.g. src/{{.ModuleCrate}}.rs.tmpl).
//
// Available fields:
//
//	.ModuleName   the Dagger module name, verbatim  ("my-module")
//	.ModuleType   the Rust type name                ("MyModule")
//	.ModuleCrate  the cargo package / crate name    ("my_module")
//
// ModuleCrate MUST stay byte-for-byte identical to toRustCrateName in
// ../../runtime/main.dang: this helper writes the [package] name into the
// scaffolded Cargo.toml at init time, and the runtime derives the binary cargo
// emits from the Dagger module name at call time. If the two ever disagree, the
// module builds and the entrypoint points at a path that does not exist. That is
// why this does not use a general-purpose case library — see
// TestNameConversionsMatchDang.
func main() {
	if err := run(os.Args[1:]); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}

var (
	reAcronymBoundary = regexp.MustCompile(`([A-Z]+)([A-Z][a-z])`)
	reCaseBoundary    = regexp.MustCompile(`([a-z0-9])([A-Z])`)
	reNonAlphanumeric = regexp.MustCompile(`[^A-Za-z0-9]+`)
)

// rustCrateName mirrors toRustCrateName in runtime/main.dang.
func rustCrateName(name string) string {
	s := reAcronymBoundary.ReplaceAllString(name, "${1}_${2}")
	s = reCaseBoundary.ReplaceAllString(s, "${1}_${2}")
	s = reNonAlphanumeric.ReplaceAllString(s, "_")
	s = strings.Trim(s, "_")
	return strings.ToLower(s)
}

// rustTypeName derives the module's Rust type name from the same segmentation
// rustCrateName uses, so the two never disagree about where a word starts.
func rustTypeName(name string) string {
	parts := strings.Split(rustCrateName(name), "_")
	for i, part := range parts {
		if part == "" {
			continue
		}
		runes := []rune(part)
		runes[0] = unicode.ToUpper(runes[0])
		parts[i] = string(runes)
	}
	return strings.Join(parts, "")
}

func run(args []string) error {
	if len(args) != 3 {
		return fmt.Errorf("usage: render-template MODULE_NAME TEMPLATE_DIR OUT_DIR")
	}

	moduleName := args[0]
	templateDir := args[1]
	outDir := args[2]

	crateName := rustCrateName(moduleName)
	if crateName == "" {
		return fmt.Errorf("module name %q has no alphanumeric characters", moduleName)
	}
	// A cargo package name may not start with a digit, and a Rust type name may
	// not either. Both are derived from the same first segment, so one check
	// covers them.
	if unicode.IsDigit(rune(crateName[0])) {
		return fmt.Errorf("module name %q yields the crate name %q, which starts with a digit", moduleName, crateName)
	}

	data := map[string]string{
		"ModuleName":  moduleName,
		"ModuleType":  rustTypeName(moduleName),
		"ModuleCrate": crateName,
	}

	return filepath.WalkDir(templateDir, func(path string, entry os.DirEntry, err error) error {
		if err != nil {
			return err
		}
		rel, err := filepath.Rel(templateDir, path)
		if err != nil {
			return err
		}
		if rel == "." {
			return nil
		}

		dstRel := strings.TrimSuffix(rel, ".tmpl")
		if strings.Contains(dstRel, "{{") {
			pathTmpl, err := template.New("path-" + rel).Parse(dstRel)
			if err != nil {
				return err
			}
			var pathBuf bytes.Buffer
			if err := pathTmpl.Execute(&pathBuf, data); err != nil {
				return err
			}
			dstRel = pathBuf.String()
		}
		dst := filepath.Join(outDir, dstRel)
		if entry.IsDir() {
			return os.MkdirAll(dst, 0o755)
		}
		if entry.Type()&os.ModeSymlink != 0 {
			return fmt.Errorf("template symlinks are not supported: %s", rel)
		}
		if err := os.MkdirAll(filepath.Dir(dst), 0o755); err != nil {
			return err
		}

		contents, err := os.ReadFile(path)
		if err != nil {
			return err
		}
		if !strings.HasSuffix(rel, ".tmpl") {
			return os.WriteFile(dst, contents, 0o644)
		}

		var buf bytes.Buffer
		tmpl, err := template.New(rel).Parse(string(contents))
		if err != nil {
			return err
		}
		if err := tmpl.Execute(&buf, data); err != nil {
			return err
		}
		return os.WriteFile(dst, buf.Bytes(), 0o644)
	})
}
