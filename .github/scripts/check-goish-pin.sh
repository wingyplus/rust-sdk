#!/bin/sh
# Fail if the goish git rev is not identical in every manifest that pins it.
#
# goish is not on crates.io, so it is pinned by rev rather than by version.
# Cargo treats two revs as two different crates: if a module and the `dagger`
# it vendors disagree, the link fails on duplicate runtime symbols (goish
# defines `panic_impl`, `_start` and friends, and there can only be one of
# each). The failure surfaces at link time in a scaffolded module, which is a
# long way from the manifest that caused it — hence this check.
#
# Runnable on its own: ./.github/scripts/check-goish-pin.sh
set -eu

# Every file carrying a `goish = { git = ..., rev = ... }` pin. The two
# templates are rendered into new modules verbatim, so their pin has to match
# the SDK crate they will be built against.
FILES="
sdk/Cargo.toml
sdk/codegen/Cargo.toml
helpers/render-template/Cargo.toml
templates/default/Cargo.toml.tmpl
templates/empty/Cargo.toml.tmpl
"

expected=""
expected_file=""
status=0

for file in $FILES; do
    if [ ! -f "$file" ]; then
        echo "::error file=$file::expected to pin goish, but the file is missing"
        status=1
        continue
    fi

    rev="$(sed -n 's|.*cogentica-ai/goish".*rev = "\([0-9a-f]\{40\}\)".*|\1|p' "$file")"

    if [ -z "$rev" ]; then
        echo "::error file=$file::no goish rev pin found"
        status=1
        continue
    fi

    # More than one pin in a single file is just as broken as two files
    # disagreeing, and would make the comparison below meaningless.
    if [ "$(printf '%s\n' "$rev" | wc -l)" -ne 1 ]; then
        echo "::error file=$file::expected exactly one goish rev pin, found $(printf '%s\n' "$rev" | wc -l)"
        status=1
        continue
    fi

    printf '%-40s %s\n' "$file" "$rev"

    if [ -z "$expected" ]; then
        expected="$rev"
        expected_file="$file"
    elif [ "$rev" != "$expected" ]; then
        echo "::error file=$file::goish rev $rev does not match $expected in $expected_file"
        status=1
    fi
done

if [ "$status" -eq 0 ]; then
    echo "All manifests pin goish at $expected."
else
    echo "goish pins are out of step; bump them together."
fi

exit "$status"
