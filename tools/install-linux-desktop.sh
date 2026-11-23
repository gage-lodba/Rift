#!/usr/bin/env bash
# Install (or remove) the Rift desktop launcher on Linux.
#
# Copies the built `rift-music` binary, the hicolor icon set and the
# `Rift.desktop` entry into the user's XDG directories, so Rift appears in the
# desktop environment's application menu with its icon.
#
# The binary and the entry belong together: the entry's Exec line is made
# absolute to the installed binary, its Icon line names the installed icon, and
# its basename (Rift.desktop) matches the window's app_id/WM_CLASS
# (`rift_desktop::APP_ID`) — that pair is what lets the compositor group the
# running window under the launcher and its icon. The repo's
# `crates/rift-desktop/tests/desktop_entry.rs` guards the entry itself.
#
# Installation is transactional: every file is staged under a temporary name in
# its destination directory, the staged entry is validated, and only then are
# the files renamed into place and the manifest written. Each destination that
# already exists is moved to a hidden backup in the same directory first. If any
# step after validation fails (a rename, a directory where a file belongs, or
# the manifest write), the files this run already published are removed, the
# previous binary/icons/entry are restored from the backups, the previous
# manifest is left untouched, and the script exits non-zero. A first-time
# install that fails therefore leaves no partial files or manifest behind, and
# a rejected entry still publishes nothing and replaces nothing.
#
# Installation records an install manifest listing every path it created.
# --uninstall deletes only those paths (and only while they are still inside
# this script's bin/data directories); without a manifest it refuses instead of
# guessing which files are Rift's.
#
# Default locations (XDG base directories; both variables must be absolute when
# set, and an unset one falls back to $HOME/.local):
#   binary:    ${XDG_BIN_HOME:-$HOME/.local/bin}/rift-music
#   entry:     ${XDG_DATA_HOME:-$HOME/.local/share}/applications/Rift.desktop
#   icons:     ${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor/<size>/apps/
#   manifest:  ${XDG_DATA_HOME:-$HOME/.local/share}/rift-desktop/install-manifest
# `--prefix DIR` instead installs into DIR/bin and DIR/share (for example
# /usr/local), keeping both trees under one explicit root.
#
# Usage:
#   tools/install-linux-desktop.sh                  # install from target/release (or debug)
#   tools/install-linux-desktop.sh --binary PATH    # install a specific built/released binary
#   tools/install-linux-desktop.sh --prefix DIR     # install under DIR/bin + DIR/share
#   tools/install-linux-desktop.sh --uninstall      # remove what the install manifest lists
#   tools/install-linux-desktop.sh --uninstall --force  # no manifest: remove only the fixed paths
#   tools/install-linux-desktop.sh --dry-run        # print the operations, change nothing
#   tools/install-linux-desktop.sh --no-cache-refresh  # skip the desktop/icon cache refresh
#
# No root is needed for the default XDG locations. Pointing --prefix at a system
# directory (for example /usr/local) is supported, but then run the script as
# whoever owns that tree (e.g. with sudo) — the script itself never escalates.
#
# Run `cargo build --release -p rift-desktop` first when using the default
# binary discovery. A downloaded release asset works too; it does not need the
# execute bit, because `install -Dm755` sets the installed mode:
#   tools/install-linux-desktop.sh --binary ~/Downloads/rift-music-linux-x86_64
set -euo pipefail

repo_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)

prefix=""
binary=""
mode="install"
dry_run=0
cache_refresh=1
force=0

# Icon sources, as "<hicolor size directory>:<asset in crates/rift-desktop/assets/icons>".
# The desktop entry's `Icon=rift-music` resolves to each installed
# <size>/apps/rift-music.png.
icons=(
    "32x32:32x32.png"
    "64x64:64x64.png"
    "128x128:128x128.png"
    "256x256:128x128@2x.png"
    "512x512:icon.png"
)
desktop_src="$repo_root/crates/rift-desktop/assets/linux/Rift.desktop"
icon_src_dir="$repo_root/crates/rift-desktop/assets/icons"

die() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}

usage() {
    cat <<'EOF'
Usage:
  tools/install-linux-desktop.sh                  # install from target/release (or debug)
  tools/install-linux-desktop.sh --binary PATH    # install a specific built/released binary
  tools/install-linux-desktop.sh --prefix DIR     # install under DIR/bin + DIR/share
  tools/install-linux-desktop.sh --uninstall      # remove what the install manifest lists
  tools/install-linux-desktop.sh --uninstall --force  # no manifest: remove only the fixed paths
  tools/install-linux-desktop.sh --dry-run        # print the operations, change nothing
  tools/install-linux-desktop.sh --no-cache-refresh  # skip the desktop/icon cache refresh

Without --prefix the XDG base directories are used:
  binary    ${XDG_BIN_HOME:-$HOME/.local/bin}/rift-music
  entry     ${XDG_DATA_HOME:-$HOME/.local/share}/applications/Rift.desktop
  icons     ${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor/
  manifest  ${XDG_DATA_HOME:-$HOME/.local/share}/rift-desktop/install-manifest
EOF
}

while [ $# -gt 0 ]; do
    case "$1" in
        --prefix)
            [ $# -ge 2 ] || die "--prefix needs a directory"
            [ -n "$2" ] || die "--prefix needs a non-empty directory"
            prefix=$2
            shift 2
            ;;
        --prefix=*)
            prefix=${1#--prefix=}
            [ -n "$prefix" ] || die "--prefix needs a non-empty directory"
            shift
            ;;
        --binary)
            [ $# -ge 2 ] || die "--binary needs a path"
            [ -n "$2" ] || die "--binary needs a non-empty path"
            binary=$2
            shift 2
            ;;
        --binary=*)
            binary=${1#--binary=}
            [ -n "$binary" ] || die "--binary needs a non-empty path"
            shift
            ;;
        --uninstall | --remove)
            mode="uninstall"
            shift
            ;;
        --force)
            force=1
            shift
            ;;
        --dry-run)
            dry_run=1
            shift
            ;;
        --no-cache-refresh)
            cache_refresh=0
            shift
            ;;
        -h | --help)
            usage
            exit 0
            ;;
        *)
            die "unknown argument '$1' (try --help)"
            ;;
    esac
done

if [ "$force" -eq 1 ] && [ "$mode" != "uninstall" ]; then
    die "--force is only valid with --uninstall"
fi

# Resolve the two destination trees. The explicit prefix keeps the long-standing
# layout (<prefix>/bin + <prefix>/share); the default honours the XDG base
# directory variables so a customised data dir still gets the menu entry/icon.
if [ -n "$prefix" ]; then
    case "$prefix" in
        /*) ;;
        *) die "the install prefix must be absolute: '$prefix'" ;;
    esac
    # Normalise duplicate/`.`/`..` path components and trailing slashes, then
    # refuse the filesystem root: `--prefix /` would otherwise install into /bin
    # and /share, and --uninstall's containment check would accept any path.
    prefix=$(readlink -m -- "$prefix") || die "could not normalise the install prefix"
    [ "$prefix" != "/" ] || die "the install prefix must not be the filesystem root"
    bin_dir="$prefix/bin"
    data_dir="$prefix/share"
else
    data_home=${XDG_DATA_HOME:-}
    bin_home=${XDG_BIN_HOME:-}
    if [ -z "$data_home" ] || [ -z "$bin_home" ]; then
        [ -n "${HOME:-}" ] || die "HOME is unset; pass --prefix DIR or set XDG_DATA_HOME and XDG_BIN_HOME"
        [ -n "$data_home" ] || data_home="$HOME/.local/share"
        [ -n "$bin_home" ] || bin_home="$HOME/.local/bin"
    fi
    case "$data_home" in
        /*) ;;
        *) die "XDG_DATA_HOME must be absolute: '$data_home'" ;;
    esac
    case "$bin_home" in
        /*) ;;
        *) die "XDG_BIN_HOME must be absolute: '$bin_home'" ;;
    esac
    data_home=$(readlink -m -- "$data_home") || die "could not normalise XDG_DATA_HOME"
    bin_home=$(readlink -m -- "$bin_home") || die "could not normalise XDG_BIN_HOME"
    [ "$data_home" != "/" ] || die "XDG_DATA_HOME must not be the filesystem root"
    [ "$bin_home" != "/" ] || die "XDG_BIN_HOME must not be the filesystem root"
    bin_dir="$bin_home"
    data_dir="$data_home"
fi

# The install manifest is line-based, so a path containing a line break cannot
# be represented safely (or removed again).
case "$bin_dir$data_dir" in
    *$'\n'* | *$'\r'*) die "install paths must not contain newline characters" ;;
esac

icon_root="$data_dir/icons/hicolor"
desktop_dest="$data_dir/applications/Rift.desktop"
installed_binary="$bin_dir/rift-music"
manifest_dir="$data_dir/rift-desktop"
manifest="$manifest_dir/install-manifest"

# Every destination path the installer creates (the manifest itself is removed
# separately, last). The fixed-path fallback for --force uses this list too, so
# it can never touch anything beyond the binary, entry and icons.
icon_dests=()
for icon in "${icons[@]}"; do
    icon_dests+=("$icon_root/${icon%%:*}/apps/rift-music.png")
done
known_paths=("$installed_binary" "${icon_dests[@]}" "$desktop_dest")

# Run a command, or print it under --dry-run. Stderr stays attached so a real
# failure is visible.
run() {
    if [ "$dry_run" -eq 1 ]; then
        printf '  would: %s\n' "$*"
    else
        "$@"
    fi
}

# The temporary staging path for a destination: a hidden file in the same
# directory, so publishing it later is an atomic rename on one filesystem.
staging_path() {
    local dest=$1
    local dir=${dest%/*}
    local base=${dest##*/}
    printf '%s/.%s.install.%s' "$dir" "$base" "$$"
}

# Quote a path for the desktop entry Exec key.
#
# The Desktop Entry Specification applies two escaping layers, in this order:
#
#   1. Exec's quoted-string layer: the value is always double-quoted, and `"`,
#      `` ` ``, `$` and `\` inside it are prefixed with one backslash.
#   2. The general Desktop Entry string layer: every backslash in the resulting
#      value is escaped again (`\` becomes `\\`).
#
# So the file ends up with FOUR backslashes for a literal `\`, and TWO for each
# of `"`, `` ` `` and `$` — written as `\\"`, `` \\` `` and `\\$`. One
# backslash is not enough: the string layer consumes it, leaving the Exec layer
# a bare reserved character and GLib (`gio launch`) rejects the entry. The two
# stages below are kept separate to make that ordering explicit.
#
# `%` is a field-code marker in Exec, and GLib rejects the `%%` workaround as
# well, so a `%` in the installed binary path is rejected before install.
exec_key_value() {
    local value=$1
    # Stage 1: Exec quoted-string layer.
    value=${value//\\/\\\\}
    value=${value//\"/\\\"}
    value=${value//\`/\\\`}
    value=${value//\$/\\\$}
    value="\"$value\""
    # Stage 2: general Desktop Entry string layer.
    value=${value//\\/\\\\}
    printf '%s' "$value"
}

# Stage the desktop entry (the template with Exec rebased to the installed
# binary) at a temporary path in the applications directory. It is validated
# there before being renamed to $desktop_dest.
stage_desktop_entry() {
    local tmp=$1
    local exec_value
    exec_value=$(exec_key_value "$installed_binary")
    if [ "$dry_run" -eq 1 ]; then
        printf '  would: install %s -> %s (Exec=%s)\n' "$desktop_src" "$desktop_dest" "$exec_value"
        return 0
    fi
    local trimmed
    # The value goes through the environment, not `awk -v`: `-v` processes
    # escape sequences, which would mangle a path containing backslashes.
    trimmed=$(EXEC_VALUE=$exec_value awk '
        /^Exec=/ { print "Exec=" ENVIRON["EXEC_VALUE"]; next }
        { print }
    ' "$desktop_src")
    install -Dm644 /dev/stdin "$tmp" <<<"$trimmed"
}

# Write the install manifest atomically (temp file in the same directory, then
# rename). It lists every path the install created, one absolute path per line;
# `#` lines are comments. --uninstall reads it and removes it last, so an
# interrupted uninstall can be retried with the manifest still intact. The
# rename is the last publication step: the caller writes the manifest only after
# every file is in place, and rolls the files back if this function fails here.
write_manifest() {
    local path
    if [ "$dry_run" -eq 1 ]; then
        printf '  would: write install manifest %s\n' "$manifest"
        for path in "${known_paths[@]}"; do
            printf '    %s\n' "$path"
        done
        return 0
    fi
    if ! mkdir -p -- "$manifest_dir"; then
        return 1
    fi
    manifest_tmp="$manifest_dir/.install-manifest.$$"
    if ! {
        printf '# Rift desktop launcher install manifest (format 1)\n'
        printf '# One absolute path per line; --uninstall removes these paths and then this file.\n'
        for path in "${known_paths[@]}"; do
            printf '%s\n' "$path"
        done
    } >"$manifest_tmp"; then
        return 1
    fi
    if ! chmod 0644 -- "$manifest_tmp"; then
        return 1
    fi
    # -T: replace the manifest itself and never move the temp file inside a
    # directory that happens to sit at the manifest path.
    if ! mv -fT -- "$manifest_tmp" "$manifest"; then
        return 1
    fi
    manifest_tmp=""
    return 0
}

# True when a manifest line names an absolute path inside one of the two trees
# this script installs into. Relative paths, `..` components and paths outside
# the bin/data roots are skipped, so a tampered or stale manifest cannot make
# --uninstall delete arbitrary files.
path_is_install_path() {
    local path=$1
    case "$path" in
        /*) ;;
        *) return 1 ;;
    esac
    case "$path" in
        *"/../"* | *"/./"* | */.. | */. | *//*) return 1 ;;
    esac
    case "$path" in
        "$bin_dir"/* | "$data_dir"/*) return 0 ;;
        *) return 1 ;;
    esac
}

# True when at least one of the fixed destination paths exists. Used only to
# distinguish "already uninstalled" (idempotent success) from "unknown files at
# Rift's names" (refuse without --force) when no manifest is present.
fixed_paths_exist() {
    local path
    for path in "${known_paths[@]}"; do
        if [ -e "$path" ] || [ -L "$path" ]; then
            return 0
        fi
    done
    return 1
}

refresh_caches() {
    if [ "$cache_refresh" -eq 0 ]; then
        return 0
    fi
    if [ "$dry_run" -eq 1 ]; then
        printf '  would: refresh the desktop/icon caches when their tools exist\n'
        return 0
    fi
    # Best effort: the caches are an optimisation, and the tools are optional.
    if command -v update-desktop-database >/dev/null 2>&1; then
        update-desktop-database "$data_dir/applications" >/dev/null 2>&1 || true
    fi
    if command -v gtk-update-icon-cache >/dev/null 2>&1; then
        gtk-update-icon-cache -q -t "$icon_root" >/dev/null 2>&1 || true
    fi
    # KDE keeps its own service cache; refresh it so the menu entry shows up
    # without a re-login.
    if command -v kbuildsycoca6 >/dev/null 2>&1; then
        kbuildsycoca6 --noincremental >/dev/null 2>&1 || true
    elif command -v kbuildsycoca5 >/dev/null 2>&1; then
        kbuildsycoca5 --noincremental >/dev/null 2>&1 || true
    fi
}

# Validate the staged entry before anything is published. A missing validator is
# not an error: the tool is optional (the guard tests cover the committed entry).
validate_entry() {
    local path=$1
    if [ "$dry_run" -eq 1 ]; then
        printf '  would: desktop-file-validate the staged entry for %s\n' "$desktop_dest"
        return 0
    fi
    command -v desktop-file-validate >/dev/null 2>&1 || return 0
    desktop-file-validate "$path" || die "desktop-file-validate rejected $path"
}

# Remove every staged temp file on exit (a failed validation leaves them behind
# otherwise), plus any half-written manifest. A --dry-run never removes anything:
# it never created a staged file, and it must stay write-free. The list is
# cleared only after the transaction commits. Backups are deliberately NOT
# removed here: after a failed rollback they are the only copy of the previous
# install, so rollback_install reports their paths instead.
staged_paths=()
manifest_tmp=""
# Publication transaction state: destinations this run renamed into place, and
# the parallel (destination, backup) pairs for the previous files moved aside.
# Parallel indexed arrays keep this bash-4.2 friendly and, unlike an associative
# array, deterministic to walk in reverse. transaction_open marks the window
# between the first publish and the commit, so the EXIT trap can roll back an
# unexpected exit (e.g. SIGINT) as well as the explicit failure path.
published_paths=()
backup_dests=()
backup_files=()
transaction_open=0
cleanup() {
    local path
    if [ "$dry_run" -eq 1 ]; then
        return 0
    fi
    # Last line of defence: an unexpected exit while the transaction is open
    # (signal, an untrapped failing command) still restores the previous
    # install. Expected publication failures call install_failed explicitly.
    if [ "$transaction_open" -eq 1 ]; then
        rollback_install || true
    fi
    if [ "${#staged_paths[@]}" -gt 0 ]; then
        for path in "${staged_paths[@]}"; do
            rm -f -- "$path"
        done
    fi
    if [ -n "$manifest_tmp" ]; then
        rm -f -- "$manifest_tmp"
    fi
}
trap cleanup EXIT

if [ "$mode" = "uninstall" ]; then
    [ "$dry_run" -eq 1 ] || printf 'Removing the Rift desktop launcher (binary %s, data %s)\n' "$installed_binary" "$data_dir"
    if [ -f "$manifest" ]; then
        [ -r "$manifest" ] || die "cannot read the install manifest '$manifest'"
        while IFS= read -r path || [ -n "$path" ]; do
            case "$path" in
                '' | '#'*) continue ;;
            esac
            if ! path_is_install_path "$path"; then
                printf 'warning: skipping manifest path outside the install roots: %s\n' "$path" >&2
                continue
            fi
            run rm -f -- "$path"
        done <"$manifest"
        # Remove the manifest only after every listed path was processed, so a
        # failed removal leaves something to retry from.
        run rm -f -- "$manifest"
        if [ "$dry_run" -eq 0 ]; then
            rmdir -- "$manifest_dir" 2>/dev/null || true
        fi
    elif fixed_paths_exist; then
        if [ "$force" -eq 0 ]; then
            die "refusing to uninstall: no install manifest at '$manifest', but files exist at Rift's fixed install paths (for example '$installed_binary'). Re-run with --force to remove only those fixed paths."
        fi
        printf 'warning: no install manifest at %s; --force removes only the fixed install paths (binary, entry and icons), never anything else.\n' "$manifest" >&2
        for path in "${known_paths[@]}"; do
            run rm -f -- "$path"
        done
    else
        # Neither a manifest nor any fixed path: this install is already gone.
        # Succeed (rather than fail) so repeated --uninstall is idempotent.
        [ "$dry_run" -eq 1 ] || printf 'Nothing to remove: no install manifest and none of the fixed install paths exist.\n'
        exit 0
    fi
    refresh_caches
    [ "$dry_run" -eq 1 ] || printf 'Removed (the desktop environment may need a re-login to drop the cached entry).\n'
    exit 0
fi

# --- install -----------------------------------------------------------------

if [ -n "$binary" ]; then
    # An explicitly supplied file may be a downloaded release asset whose mode
    # is 0644; install -Dm755 below sets the executable bit. Only auto-discovered
    # build outputs have to be executable already.
    [ -f "$binary" ] || die "no regular file at '$binary'"
    [ -r "$binary" ] || die "'$binary' is not readable"
else
    for candidate in "$repo_root/target/release/rift-music" "$repo_root/target/debug/rift-music"; do
        if [ -f "$candidate" ] && [ -x "$candidate" ]; then
            binary=$candidate
            break
        fi
    done
    [ -n "$binary" ] || die "no rift-music binary found; build it (cargo build --release -p rift-desktop) or pass --binary PATH"
fi
if [ "$(readlink -f -- "$binary")" = "$(readlink -f -- "$installed_binary" 2>/dev/null || true)" ]; then
    die "'$binary' is the installed copy itself; pass the build output instead"
fi
# Refuse a `%` before writing anything: Exec treats it as a field code, and this
# installer does not implement the `%%` escape, so such an entry would not launch
# reliably across desktop environments.
case "$installed_binary" in
    *%*)
        die "the installed binary path '$installed_binary' contains '%'; desktop launchers expand '%' in Exec as a field code and this installer does not emit the '%%' escape, so choose an install path without '%'"
        ;;
esac

[ -f "$desktop_src" ] || die "missing desktop entry template: $desktop_src"
for icon in "${icons[@]}"; do
    [ -f "$icon_src_dir/${icon#*:}" ] || die "missing icon asset: $icon_src_dir/${icon#*:}"
done

printf 'Installing Rift from %s (binary: %s; data: %s)\n' "$binary" "$bin_dir" "$data_dir"

# --- publication transaction -------------------------------------------------
#
# Renaming the staged files one at a time is not atomic across files, so a
# failure halfway through (ENOSPC, a directory where a file belongs, a failing
# manifest write) would otherwise leave some new files published without a
# matching manifest. Instead every destination that already exists is parked at
# a hidden backup in its own directory before it is replaced; on failure the
# files this run published are removed and the backups are moved back, which
# restores the previous contents *and modes* (a rename does not rewrite a file).
# The backups are deleted only after the manifest rename has committed.
#
# Rollback is explicit rather than an ERR trap: a trap also fires for failures
# that are deliberately tested (`if ! cmd`) and cannot report the original error
# after restore warnings. Every fallible step below goes through publish_file or
# the checked write_manifest instead.

# Roll the publication back: remove the files this run published (newest first)
# and restore the previous files from their backups. Best effort by design and
# never fatal here: a failed removal/restore warns and leaves the backup in
# place, while the caller still reports and exits on the original error. Returns
# non-zero only when something could not be removed or restored.
rollback_install() {
    local status=0
    local index path backup dest

    if [ "${#published_paths[@]}" -gt 0 ]; then
        index=$((${#published_paths[@]} - 1))
        while [ "$index" -ge 0 ]; do
            path=${published_paths[index]}
            if ! rm -f -- "$path"; then
                printf 'warning: rollback could not remove the newly published %s\n' "$path" >&2
                status=1
            fi
            index=$((index - 1))
        done
    fi
    published_paths=()

    if [ "${#backup_files[@]}" -gt 0 ]; then
        index=$((${#backup_files[@]} - 1))
        while [ "$index" -ge 0 ]; do
            backup=${backup_files[index]}
            dest=${backup_dests[index]}
            if [ -e "$backup" ] || [ -L "$backup" ]; then
                if ! mv -fT -- "$backup" "$dest"; then
                    printf 'warning: rollback could not restore %s; the previous file is kept at %s\n' "$dest" "$backup" >&2
                    status=1
                fi
            fi
            index=$((index - 1))
        done
    fi
    backup_dests=()
    backup_files=()
    transaction_open=0

    return "$status"
}

# Abort the publication: roll back, then fail with the original reason. The
# rollback's warnings reach stderr first, so a partial restore is visible, and
# the exit status stays non-zero either way.
install_failed() {
    local reason=$1
    if rollback_install; then
        die "$reason; the install was rolled back and the previous install restored"
    else
        die "$reason; the rollback was incomplete (see the warnings above)"
    fi
}

# Publish one staged file at its final destination. An existing destination is
# moved to a unique hidden backup in the same directory first; a directory where
# one of our files belongs is refused instead of renamed away, because this
# installer did not create it and a successful install would leave it misplaced.
# `mv -T` keeps both renames file-for-file: the destination is never treated as
# a container directory.
publish_file() {
    local src=$1
    local dest=$2
    local dir=${dest%/*}
    local base=${dest##*/}
    local backup

    if [ -d "$dest" ] && [ ! -L "$dest" ]; then
        install_failed "could not publish '$dest': the destination is a directory, not a file"
    fi
    if [ -e "$dest" ] || [ -L "$dest" ]; then
        backup=$(mktemp -- "$dir/.$base.backup.XXXXXX") \
            || install_failed "could not publish '$dest': could not create a backup path"
        if ! mv -fT -- "$dest" "$backup"; then
            rm -f -- "$backup" || true
            install_failed "could not publish '$dest': could not move the previous file aside"
        fi
        backup_dests+=("$dest")
        backup_files+=("$backup")
    fi
    if ! mv -fT -- "$src" "$dest"; then
        install_failed "could not publish '$dest': the staged rename failed"
    fi
    published_paths+=("$dest")
}

# The transaction committed (the manifest was renamed into place), so the
# previous files' backups are just hidden junk. Removal is best effort and never
# fails the install; a warning names anything that survived so it can be deleted
# by hand.
discard_backups() {
    local backup
    if [ "${#backup_files[@]}" -gt 0 ]; then
        for backup in "${backup_files[@]}"; do
            if ! rm -f -- "$backup"; then
                printf 'warning: could not remove the temporary backup %s (the install itself succeeded)\n' "$backup" >&2
            fi
        done
    fi
    backup_dests=()
    backup_files=()
}

# Stage every file under a temporary name in its destination directory, then
# validate the staged entry, and only then rename the files into place. Staging
# keeps the reinstall-over-a-running-binary property (a rename over a running
# executable is fine on Linux; writing it in place fails with ETXTBSY) and makes
# the validation rollback trivial: a rejected entry leaves the previously
# installed binary, icons, entry and manifest untouched.
install_tmp=$(staging_path "$installed_binary")
staged_paths+=("$install_tmp")
run install -Dm755 "$binary" "$install_tmp"

for icon in "${icons[@]}"; do
    size=${icon%%:*}
    src=${icon#*:}
    icon_dest="$icon_root/$size/apps/rift-music.png"
    icon_tmp=$(staging_path "$icon_dest")
    staged_paths+=("$icon_tmp")
    run install -Dm644 "$icon_src_dir/$src" "$icon_tmp"
done

# The staged entry has to keep a `.desktop` suffix of its own:
# desktop-file-validate rejects a file name without one.
entry_tmp="$(staging_path "$desktop_dest").desktop"
staged_paths+=("$entry_tmp")
stage_desktop_entry "$entry_tmp"

if [ "$dry_run" -eq 1 ]; then
    validate_entry "$entry_tmp"
    printf '  would: rename the staged binary, icons and entry into place\n'
    write_manifest
else
    # Validate before publishing anything — in particular before replacing the
    # installed binary and icons — so a rejected entry cannot leave a partial
    # install and is never recorded in the manifest.
    validate_entry "$entry_tmp"
    transaction_open=1
    for icon in "${icons[@]}"; do
        size=${icon%%:*}
        icon_dest="$icon_root/$size/apps/rift-music.png"
        publish_file "$(staging_path "$icon_dest")" "$icon_dest"
    done
    publish_file "$install_tmp" "$installed_binary"
    publish_file "$entry_tmp" "$desktop_dest"
    # Every file is published; only now does the manifest claim ownership of it.
    # A failed manifest write rolls the published files back too, so an install
    # can never exist as files without the manifest that owns them.
    if ! write_manifest; then
        install_failed "could not write the install manifest '$manifest'"
    fi
    # The manifest rename committed the install; a signal from here on must not
    # undo it. Only then are the previous files' backups disposable.
    transaction_open=0
    discard_backups
    staged_paths=()
fi
refresh_caches

if [ "$dry_run" -eq 1 ]; then
    printf '\nDry run only: nothing was written.\n'
    exit 0
fi

printf '\nInstalled:\n'
printf '  %s\n' "$installed_binary"
for icon in "${icons[@]}"; do
    printf '  %s\n' "$icon_root/${icon%%:*}/apps/rift-music.png"
done
printf '  %s\n' "$desktop_dest"
printf '  %s\n' "$manifest"
printf '\nRift should now appear in the application menu ("Rift"); launch it there or run `%s`.\n' "$installed_binary"
printf 'Remove it again with: %s --uninstall\n' "${BASH_SOURCE[0]}"
