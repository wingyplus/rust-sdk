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

// rustKeywords is every word Rust reserves as of the 2024 edition: the strict
// keywords, the reserved-for-future-use set, and the edition-specific additions.
// Raw identifiers are not an escape hatch for all of them — `crate`, `self`,
// `super` and `Self` cannot be written `r#`-prefixed — so a name landing here is
// rejected rather than escaped.
var rustKeywords = map[string]bool{
	// Strict.
	"as": true, "break": true, "const": true, "continue": true, "crate": true,
	"else": true, "enum": true, "extern": true, "false": true, "fn": true,
	"for": true, "if": true, "impl": true, "in": true, "let": true,
	"loop": true, "match": true, "mod": true, "move": true, "mut": true,
	"pub": true, "ref": true, "return": true, "self": true, "Self": true,
	"static": true, "struct": true, "super": true, "trait": true, "true": true,
	"type": true, "unsafe": true, "use": true, "where": true, "while": true,
	// Strict from the 2018 edition on.
	"async": true, "await": true, "dyn": true,
	// Reserved for future use.
	"abstract": true, "become": true, "box": true, "do": true, "final": true,
	"macro": true, "override": true, "priv": true, "typeof": true,
	"unsized": true, "virtual": true, "yield": true,
	// Reserved from the 2018 edition on.
	"try": true,
	// Reserved from the 2024 edition on.
	"gen": true,
}

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

	// The type name becomes a `struct` declaration in the generated main.rs, so
	// a reserved word there is a hard compile error. In practice only `Self`
	// reaches this — every other Rust keyword is lowercase, and the type name is
	// capitalized per segment — but checking the whole set keeps the rule honest
	// if the derivation ever changes.
	//
	// The crate name is deliberately *not* checked against the same set. It only
	// ever appears as a cargo package name, a bin target name and a filename,
	// never as a Rust identifier: a module named `crate`, `self`, `type` or
	// `move` builds and produces a binary. `cargo new` refuses those names
	// because it also creates a lib target, which these templates do not.
	typeName := rustTypeName(moduleName)
	if rustKeywords[typeName] {
		return fmt.Errorf("module name %q yields the Rust type name %q, which is a reserved keyword; pick another name", moduleName, typeName)
	}

	data := map[string]string{
		"ModuleName":  moduleName,
		"ModuleType":  typeName,
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
