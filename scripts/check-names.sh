#!/bin/sh
# The names-and-paths check: AGENTS.md, "Hard rule: no borrowed names, no borrowed paths".
#
#   sh scripts/check-names.sh          (in the dev container: `just names-check`)
#
# It reads every file git would commit: the tracked files, plus the untracked files that
# .gitignore does not exclude, so a new file is caught before it is added. Two checks:
#
# 1. Absolute paths into a home directory or another checkout. Always on. It flags a drive-letter
#    path (except the Windows system folders), a Linux or macOS home directory in any spelling,
#    Git Bash and WSL included, a WSL share, and a home-relative path into a projects or documents
#    folder. Container paths such as /work, /target, /cargo and /usr are not homes and pass, and
#    so do web URLs.
# 2. Private names. On when a denylist is available: one name per line, blank lines and lines
#    starting with # ignored, matched ignoring case. A name of five characters or more matches
#    anywhere; a shorter one matches only as a whole word, so it does not fire inside base64 data.
#    The list is never in the repository, because listing the names would put them there. It comes
#    from, in this order:
#      - the variable APPRICOT_NAMES_DENYLIST (CI fills it from a repository secret);
#      - the file info/names-denylist in the git common dir (`git rev-parse --git-common-dir`),
#        which is local and never tracked.
#    With neither, it says so and runs the path check alone.
#
# File contents and file paths are both checked. A binary file is checked through its runs of
# printable characters, which is where embedded metadata sits.
#
# A hit prints `file:line` (for a binary file, the file alone) and never the matched text, so the
# output is safe in a public CI log. A hit in a file's own path prints the path's position in the
# file list instead, since the path itself holds the name.
#
# Exit status: 0 clean, 1 at least one hit, 2 the check could not run.
#
# It needs git, and GNU grep for -o and -I; the dev image has both.

set -eu

die() {
    printf 'names-check: %s\n' "$*" >&2
    exit 2
}

root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd) || die 'cannot find the repository root'

# The dev container sees a bind-mounted checkout as owned by root, and git then refuses to read
# it ("dubious ownership"). This script only reads, and only the checkout it belongs to.
g() {
    git -C "$root" -c safe.directory="$root" "$@"
}

g rev-parse --git-dir >/dev/null 2>&1 ||
    die "git cannot read the checkout here. In a git worktree whose git dir lies outside the" \
        'container, set GIT_DIR and GIT_WORK_TREE, or run the check from the main checkout.'

tmp=$(mktemp -d) || die 'mktemp failed'
trap 'rm -rf "$tmp"' EXIT
trap 'exit 2' HUP INT TERM

# grep with "no match" (status 1) as success, and a real error (status 2) fatal.
grep_ok() {
    grep "$@" || [ "$?" -eq 1 ] || die "grep failed: $*"
}

g -c core.quotepath=off ls-files --cached --others --exclude-standard >"$tmp/files" ||
    die 'git ls-files failed'

# --- the denylist -------------------------------------------------------------------------------

source=''
if [ -n "${APPRICOT_NAMES_DENYLIST:-}" ]; then
    printf '%s\n' "$APPRICOT_NAMES_DENYLIST" >"$tmp/raw"
    source='the variable APPRICOT_NAMES_DENYLIST'
else
    common=$(g rev-parse --git-common-dir) || die 'git rev-parse --git-common-dir failed'
    case $common in
        /* | [A-Za-z]:*) ;;
        *) common=$root/$common ;;
    esac
    if [ -f "$common/info/names-denylist" ]; then
        cat -- "$common/info/names-denylist" >"$tmp/raw"
        source='info/names-denylist in the git common dir'
    fi
fi

: >"$tmp/long"
: >"$tmp/short"
if [ -n "$source" ]; then
    tr -d '\r' <"$tmp/raw" |
        sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//' -e '/^#/d' -e '/^$/d' >"$tmp/names"
    # Long names: fixed strings. Short names: whole words, escaped for an extended regex.
    awk 'length($0) >= 5' "$tmp/names" >"$tmp/long"
    awk 'length($0) < 5' "$tmp/names" |
        sed -e 's/[]\.[(){}*+?^$|]/\\&/g' \
            -e 's/.*/(^|[^[:alnum:]])&([^[:alnum:]]|$)/' >"$tmp/short"
    count=$(wc -l <"$tmp/names" | tr -d ' ')
    if [ "$count" -eq 0 ]; then
        source=''
    fi
fi

if [ -n "$source" ]; then
    printf 'names-check: %s names from %s, and the path check.\n' "$count" "$source"
else
    printf '%s\n' 'names-check: no denylist (APPRICOT_NAMES_DENYLIST is empty and' \
        'info/names-denylist does not exist in the git common dir): the path check only.'
fi

# --- the patterns -------------------------------------------------------------------------------

# Candidate absolute paths, one extended regex per line, pulled out with grep -o. A drive-letter
# path needs two name characters after the separator, so an escape such as "x:\n" in a string
# literal does not look like one.
cat >"$tmp/paths" <<'EOF'
(^|[^A-Za-z0-9_])[A-Za-z]:[\/]+[A-Za-z0-9._$~-]{2,}
[^[:space:]"'<>()]*/(home|[Uu]sers)/[A-Za-z0-9._-]+
\\\\wsl(\$|\.localhost)\\
(^|[^A-Za-z0-9_])~[\/]+(Documents|Desktop|Downloads|Projects|projects|repos|source|src|code)([\/]|$)
EOF

# Candidates that are fine: the Windows system folders (a match stops at a space, so
# "Program Files" arrives as "Program"), and web URLs. Applied to grep -n -o output, `N:match`.
cat >"$tmp/allowed" <<'EOF'
^[0-9]+:[^A-Za-z0-9_]?[A-Za-z]:[\/]+([Pp]rogram|[Pp]rogram[Dd]ata|[Ww]indows)$
^[0-9]+:.*(https?|wss?|ftp)://
EOF

# Writes to $tmp/lines the numbers of the lines of a text file that hold a hit, one per line.
# The matched text goes to scratch files only; it is never printed.
text_hits() {
    grep_ok -n -o -E -f "$tmp/paths" -- "$1" >"$tmp/raw-paths"
    grep_ok -v -E -f "$tmp/allowed" "$tmp/raw-paths" >"$tmp/out"
    if [ -s "$tmp/long" ]; then
        grep_ok -n -i -F -f "$tmp/long" -- "$1" >>"$tmp/out"
    fi
    if [ -s "$tmp/short" ]; then
        grep_ok -n -i -E -f "$tmp/short" -- "$1" >>"$tmp/out"
    fi
    cut -d: -f1 "$tmp/out" | sort -un >"$tmp/lines"
}

# Writes to $tmp/found how many hits a binary file's printable runs hold. Paths are looked for
# in runs of 12 characters or more only: random bytes make short path-shaped runs.
binary_hits() {
    tr -c '[:print:]' '[\n*]' <"$1" >"$tmp/printable"
    grep_ok -E '.{5,}' "$tmp/printable" >"$tmp/runs"
    grep_ok -E '.{12,}' "$tmp/runs" >"$tmp/long-runs"
    grep_ok -n -o -E -f "$tmp/paths" "$tmp/long-runs" >"$tmp/raw-paths"
    grep_ok -v -E -f "$tmp/allowed" "$tmp/raw-paths" >"$tmp/out"
    if [ -s "$tmp/long" ]; then
        grep_ok -i -F -f "$tmp/long" "$tmp/runs" >>"$tmp/out"
    fi
    if [ -s "$tmp/short" ]; then
        grep_ok -i -E -f "$tmp/short" "$tmp/runs" >>"$tmp/out"
    fi
    wc -l <"$tmp/out" | tr -d ' ' >"$tmp/found"
}

# --- the check ----------------------------------------------------------------------------------

hits=0

# File paths. A path that holds a hit is reported by its position in the file list, never by
# itself, and so are the content hits inside that file.
grep_ok -n -o -E -f "$tmp/paths" "$tmp/files" >"$tmp/raw-paths"
grep_ok -v -E -f "$tmp/allowed" "$tmp/raw-paths" >"$tmp/path-hits"
if [ -s "$tmp/long" ]; then
    grep_ok -n -i -F -f "$tmp/long" "$tmp/files" >>"$tmp/path-hits"
fi
if [ -s "$tmp/short" ]; then
    grep_ok -n -i -E -f "$tmp/short" "$tmp/files" >>"$tmp/path-hits"
fi
cut -d: -f1 "$tmp/path-hits" | sort -un >"$tmp/bad-paths"
while IFS= read -r n; do
    printf 'path %s of `git ls-files --cached --others --exclude-standard`\n' "$n"
    hits=$((hits + 1))
done <"$tmp/bad-paths"

# File contents.
n=0
while IFS= read -r f; do
    n=$((n + 1))
    p=$root/$f
    # Gone from the work tree (deleted, not yet committed), not a regular file, or empty.
    if [ ! -f "$p" ] || [ ! -s "$p" ]; then
        continue
    fi
    label=$f
    if grep -q -x -F -- "$n" "$tmp/bad-paths"; then
        label="path $n of the file list"
    fi
    kind=text
    grep -q -I '' -- "$p" || case $? in
        1) kind=binary ;;
        *) die "cannot read $label" ;;
    esac
    if [ "$kind" = text ]; then
        text_hits "$p"
        while IFS= read -r line; do
            printf '%s:%s\n' "$label" "$line"
            hits=$((hits + 1))
        done <"$tmp/lines"
    else
        binary_hits "$p"
        if [ "$(cat "$tmp/found")" -gt 0 ]; then
            printf '%s: in its binary content\n' "$label"
            hits=$((hits + 1))
        fi
    fi
done <"$tmp/files"

if [ "$hits" -gt 0 ]; then
    printf 'names-check: %s hit(s). A private name, or an absolute path into a home or\n' "$hits"
    printf '%s\n' 'another checkout, is in the tree. See AGENTS.md, "Hard rule: no borrowed' \
        'names, no borrowed paths".'
    exit 1
fi
printf 'names-check: clean, %s files.\n' "$n"
