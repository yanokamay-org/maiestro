#!/usr/bin/env bash
#
# Release pipeline for mAIestro Code. One release (one version, one tag, one
# GitHub Release) carries the universal macOS .dmg (Apple silicon + Intel) AND
# the Windows installer, built on two machines: the .dmg on the release Mac,
# the NSIS installer on a Windows PC (Git Bash). Either machine can publish
# first; the release goes public only once every expected asset is attached.
# Every command is idempotent, so a partially-failed release resumes by
# re-running it:
#
#   release.sh bump <patch|minor|major|X.Y.Z>
#       Bump the version in backend/tauri.conf.json, package.json,
#       backend/Cargo.toml, and backend/Cargo.lock, then assert they agree.
#       Does NOT commit — the caller commits.
#
#   release.sh build [--platform macos|windows]
#       macos:   source .env.release, validate the signing identity, `tauri
#                build` the universal (arm64 + x86_64) app, and verify both
#                slices / the signature / Gatekeeper assessment /
#                notarization staple. tauri only notarizes the .app, so this
#                also submits + staples the .dmg (which publish requires);
#                already-stapled artifacts are skipped.
#       windows: build the x64 NSIS installer. It is UNSIGNED — Authenticode
#                signing is issue #224 (an ARM64 installer is #232).
#
#   release.sh publish [--platform macos|windows] [--notes-file <file>] [--prerelease]
#       Tag vX.Y.Z (or check the existing tag is HEAD), find or create the
#       *draft* GitHub Release (REST API, not `gh`), upload this platform's
#       assets, then finalize. Each step is skipped if already done.
#
#   release.sh finalize [--allow-missing <asset-key>]...
#       Publish the draft if every expected asset is attached; otherwise list
#       what's missing and leave it a draft. --allow-missing publishes without
#       that asset and says so in the release notes.
#
# --platform defaults to the host: Darwin -> macos, MINGW/MSYS -> windows.
#
# Why signing matters on macOS (beyond distribution): macOS binds a Keychain
# item's "Always Allow" decision to the app's *designated requirement*. For an
# ad-hoc / unsigned build that requirement is just the binary's cdhash, which
# changes on every rebuild — so the access prompt comes back every launch. A
# stable Developer ID signature anchors the requirement to the certificate
# instead, so "Always Allow" persists across releases. (Windows Credential
# Manager access doesn't depend on a signature; there signing is only about
# download trust / SmartScreen.)
#
# Requirements:
#   * macOS: a "Developer ID Application" certificate in the login keychain
#     (NOT "Apple Development" — that one is for local testing only and the app
#     will not launch on other people's Macs), plus notarization credentials
#     (see below). Without them the build is signed but not notarized, and
#     Gatekeeper will block it on other machines. And the Rust targets
#     aarch64-apple-darwin and x86_64-apple-darwin, for the universal binary.
#   * Windows: Git Bash, Python 3, and the Rust target x86_64-pc-windows-msvc.
#   * For `publish` / `finalize`: a GITHUB_TOKEN with contents:write on this
#     repo, on whichever machine runs them.
#
# Configuration lives in a gitignored .env.release at the repo root (see
# .env.release.example). The script sources it, so you never export by hand.
#
#   APPLE_SIGNING_IDENTITY   e.g. "Developer ID Application: Your Name (CCMY5ZR77Q)"  (macOS)
#   GITHUB_TOKEN             fine-grained PAT, contents:write on this repo
#
# Notarization (macOS) — provide EITHER an App Store Connect API key:
#   APPLE_API_ISSUER, APPLE_API_KEY, APPLE_API_KEY_PATH
# OR an Apple ID app-specific password:
#   APPLE_ID, APPLE_PASSWORD, APPLE_TEAM_ID
#
# List available identities with:  security find-identity -v -p codesigning

set -euo pipefail
# Let a failure inside $(…) abort too (bash ≥ 4.4; macOS's bash 3.2 lacks it).
shopt -s inherit_errexit 2>/dev/null || true
cd "$(dirname "$0")/.."

# --- shared helpers ---------------------------------------------------------

die() { echo "error: $*" >&2; exit 1; }

# Load signing identity + notarization secrets from the gitignored env file.
source_env() {
  if [[ -f .env.release ]]; then
    set -a
    # shellcheck disable=SC1091
    source .env.release
    set +a
  fi
}

# The Python interpreter, resolved once. On Windows it may be `python3`, only
# `python`, only the `py` launcher, or a Microsoft Store alias stub that prints
# an install hint instead of running — so each candidate is test-run rather
# than just looked up. An array, not a nameref, so this stays bash-3.2-safe.
PY=()
resolve_python() {
  local cand
  for cand in python3 python "py -3"; do
    # shellcheck disable=SC2086
    if $cand -c 'import sys; sys.exit(0 if sys.version_info >= (3, 6) else 1)' >/dev/null 2>&1; then
      read -r -a PY <<<"$cand"
      return 0
    fi
  done
  die "Python 3 not found (tried python3, python, py -3)"
}

# Run the resolved Python. UTF-8 mode makes open() and stdio UTF-8 on Windows
# too (tauri.conf.json has a ©). Windows Python also writes CRLF to a pipe,
# which would leave a stray \r on every $(…) capture — so strip it.
pyrun() {
  PYTHONUTF8=1 "${PY[@]}" "$@" | tr -d '\r'
}

# The platform this machine releases: macos or windows.
host_platform() {
  case "$(uname -s)" in
    Darwin) echo macos ;;
    MINGW*|MSYS*|CYGWIN*) echo windows ;;
    *) die "unsupported host $(uname -s) — release from macOS or Windows (Git Bash)" ;;
  esac
}

check_platform() {
  case "$1" in
    macos|windows) ;;
    *) die "unknown platform: $1 (want macos|windows)" ;;
  esac
  [[ "$1" == "$(host_platform)" ]] \
    || die "can't build/publish $1 artifacts on this $(host_platform) host"
}

# The version is single-sourced from tauri.conf.json.
read_version() {
  pyrun -c 'import json;print(json.load(open("backend/tauri.conf.json"))["version"])'
}

# --- the release's expected asset set ---------------------------------------
#
# Every published release must carry one asset per key below; `finalize` only
# publishes when all are attached. Adding a platform = adding a key here and to
# the case statements that follow.

ALL_ASSET_KEYS="macos windows-x64"

WIN_TRIPLES="x86_64-pc-windows-msvc"

# The macOS build is one universal binary: Tauri compiles both slices and
# lipo-merges them, so both targets must be installed.
MAC_TARGET="universal-apple-darwin"
MAC_TRIPLES="aarch64-apple-darwin x86_64-apple-darwin"

platform_asset_keys() {
  case "$1" in
    macos)   echo "macos" ;;
    windows) echo "windows-x64" ;;
  esac
}

# The local bundle file for an asset key.
asset_local_glob() {
  local key="$1" version="$2"
  case "$key" in
    macos)         echo "backend/target/$MAC_TARGET/release/bundle/dmg/mAIestro Code_${version}_universal.dmg" ;;
    windows-x64)   echo "backend/target/x86_64-pc-windows-msvc/release/bundle/nsis/mAIestro Code_${version}_x64-setup.exe" ;;
  esac
}

# The uploaded asset's name for a local file (or glob). Uploads are hyphenated:
# the space in "mAIestro Code_…" isn't URL-safe and GitHub would rename it
# anyway. The Windows installer is tagged "-unsigned" until #224 signs it.
asset_remote_name() {
  local key="$1" name
  name="$(basename "$2")"
  name="${name// /-}"
  case "$key" in
    windows-*) name="${name%-setup.exe}-unsigned-setup.exe" ;;
  esac
  echo "$name"
}

# The uploaded asset's name, as an fnmatch pattern.
asset_remote_pattern() {
  local key="$1" version="$2"
  asset_remote_name "$key" "$(asset_local_glob "$key" "$version")"
}

asset_content_type() {
  case "$1" in
    macos) echo "application/x-apple-diskimage" ;;
    *)     echo "application/vnd.microsoft.portable-executable" ;;
  esac
}

# Resolve an asset key to its built file, or print nothing.
asset_local_file() {
  local key="$1" version="$2" pattern f
  pattern="$(asset_local_glob "$key" "$version")"
  # Split on the glob only, not the spaces in the file name.
  local IFS=$'\n'
  for f in $(compgen -G "$pattern" || true); do
    [[ -f "$f" ]] && { echo "$f"; return 0; }
  done
}

# Check a built asset is fit to ship. macOS: notarized (stapled). Windows: the
# installer exists and is non-empty — they ship unsigned until #224 adds an
# Authenticode check here.
verify_asset() {
  local key="$1" file="$2"
  case "$key" in
    macos)
      xcrun stapler validate "$file" >/dev/null 2>&1 \
        || die "$file is not notarized (stapler validate failed) — refusing to publish"
      ;;
    windows-*)
      [[ -s "$file" ]] || die "$file is empty — rebuild it"
      ;;
  esac
}

# Populate the global NOTARY_AUTH_ARGS array with the `xcrun notarytool` auth
# flags for whichever credentials are present (App Store Connect API key
# preferred, Apple ID app-specific password as fallback). Returns non-zero if
# neither is configured. A global (not a bash-4 nameref) so this stays 3.2-safe.
NOTARY_AUTH_ARGS=()
notarytool_auth_args() {
  if [[ -n "${APPLE_API_KEY:-}" && -n "${APPLE_API_ISSUER:-}" && -n "${APPLE_API_KEY_PATH:-}" ]]; then
    NOTARY_AUTH_ARGS=(--key "$APPLE_API_KEY_PATH" --key-id "$APPLE_API_KEY" --issuer "$APPLE_API_ISSUER")
  elif [[ -n "${APPLE_ID:-}" && -n "${APPLE_PASSWORD:-}" && -n "${APPLE_TEAM_ID:-}" ]]; then
    NOTARY_AUTH_ARGS=(--apple-id "$APPLE_ID" --password "$APPLE_PASSWORD" --team-id "$APPLE_TEAM_ID")
  else
    return 1
  fi
}

# Notarize + staple a standalone artifact (e.g. the .dmg). Tauri notarizes and
# staples the .app inside the bundle, but the disk image wrapping it gets no
# ticket of its own — so `stapler validate <dmg>` (which `publish` enforces)
# fails until we submit the dmg itself. Idempotent: skips if already stapled.
notarize_and_staple() {
  local artifact="$1"
  if xcrun stapler validate "$artifact" >/dev/null 2>&1; then
    echo "Already notarized: $(basename "$artifact") ✓"
    return 0
  fi
  if ! notarytool_auth_args; then
    echo "warning: no notarization credentials — $(basename "$artifact") left un-notarized;" >&2
    echo "         'publish' will refuse it. Set APPLE_API_* or APPLE_ID/APPLE_PASSWORD/APPLE_TEAM_ID." >&2
    return 1
  fi
  echo "Notarizing $(basename "$artifact")…"
  xcrun notarytool submit "$artifact" "${NOTARY_AUTH_ARGS[@]}" --wait
  xcrun stapler staple "$artifact"
}

usage() {
  cat >&2 <<'EOF'
usage: scripts/release.sh <command>

  bump <patch|minor|major|X.Y.Z>     bump version across all manifests
  build [--platform P]               macos: build, sign, notarize, verify the .dmg
                                     windows: build the x64 installer (unsigned)
  publish [--platform P] [--notes-file <file>] [--prerelease]
                                     tag, find/create the draft release, upload
                                     this platform's assets, then finalize
  finalize [--allow-missing <key>]...
                                     publish the draft once every asset is attached

P is macos or windows (default: this host). Asset keys: macos windows-x64.
Each command is idempotent; re-run to resume a partial release.
EOF
}

# --- bump -------------------------------------------------------------------

cmd_bump() {
  local spec="${1:-}"
  [[ -n "$spec" ]] || die "bump needs a spec: patch | minor | major | X.Y.Z"

  local current new
  current="$(read_version)"
  new="$(pyrun - "$current" "$spec" <<'PY'
import re, sys
cur, spec = sys.argv[1], sys.argv[2]
if re.fullmatch(r'\d+\.\d+\.\d+', spec):
    print(spec); sys.exit(0)
try:
    maj, minr, pat = (int(x) for x in cur.split('.'))
except ValueError:
    sys.exit(f"error: current version {cur!r} is not X.Y.Z")
if spec == 'major':   maj, minr, pat = maj + 1, 0, 0
elif spec == 'minor': minr, pat = minr + 1, 0
elif spec == 'patch': pat = pat + 1
else: sys.exit(f"error: unknown bump spec {spec!r} (want patch|minor|major|X.Y.Z)")
print(f'{maj}.{minr}.{pat}')
PY
)"

  echo "Bumping $current -> $new"

  # Format-preserving edits: replace only the version token in each file.
  # Cargo.lock's `maiestro` entry is edited directly rather than via `cargo`:
  # `maiestro` is the root workspace member (nothing depends on it), so its
  # version can be rewritten in place without re-resolving the graph — which
  # would need the network for platform-only deps not in the local cache.
  # newline="" keeps the files' LF endings on Windows too.
  pyrun - "$new" <<'PY'
import re, sys
new = sys.argv[1]
edits = [
    ("backend/tauri.conf.json", r'("version"\s*:\s*")[^"]*(")', rf'\g<1>{new}\g<2>'),
    ("package.json",            r'("version"\s*:\s*")[^"]*(")', rf'\g<1>{new}\g<2>'),
    ("backend/Cargo.toml",      r'(?m)^version = "[^"]*"',       f'version = "{new}"'),
    ("backend/Cargo.lock",      r'(?m)(^name = "maiestro"\r?\nversion = ")[^"]*(")',
                                rf'\g<1>{new}\g<2>'),
]
for path, pat, repl in edits:
    s = open(path, newline="").read()
    s2, n = re.subn(pat, repl, s, count=1)
    if n != 1:
        sys.exit(f"error: expected exactly one version field in {path}, found {n}")
    open(path, "w", newline="").write(s2)
PY

  # Assert every manifest (and the lockfile) now agree.
  pyrun - "$new" <<'PY'
import json, re, sys
want = sys.argv[1]
def cargo_lock_version():
    s = open("backend/Cargo.lock").read()
    m = re.search(r'(?m)^name = "maiestro"\nversion = "([^"]*)"', s)
    return m.group(1) if m else None
def cargo_toml_version():
    s = open("backend/Cargo.toml").read()
    m = re.search(r'(?m)^version = "([^"]*)"', s)
    return m.group(1) if m else None
found = {
    "backend/tauri.conf.json": json.load(open("backend/tauri.conf.json"))["version"],
    "package.json":            json.load(open("package.json"))["version"],
    "backend/Cargo.toml":      cargo_toml_version(),
    "backend/Cargo.lock":      cargo_lock_version(),
}
bad = {k: v for k, v in found.items() if v != want}
if bad:
    sys.exit("error: version mismatch after bump: " +
             ", ".join(f"{k}={v!r}" for k, v in bad.items()) + f" (want {want!r})")
print(f"All manifests at {want}")
PY

  echo
  echo "Bumped to $new. Review, commit, then on each release machine:"
  echo "  scripts/release.sh build && scripts/release.sh publish"
}

# --- build ------------------------------------------------------------------

cmd_build() {
  local platform; platform="$(host_platform)"
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --platform) platform="${2:-}"; shift 2 ;;
      *) die "unknown build arg: $1" ;;
    esac
  done
  check_platform "$platform"
  case "$platform" in
    macos)   build_macos ;;
    windows) build_windows ;;
  esac
}

# Die unless every given Rust target triple is installed.
require_rust_targets() {
  local installed triple
  installed="$(rustup target list --installed | tr -d '\r')"
  for triple in "$@"; do
    grep -qx "$triple" <<<"$installed" \
      || die "Rust target $triple is not installed — run: rustup target add $triple"
  done
}

# Die unless the .app's main binary carries both the arm64 and x86_64 slices,
# so a single-arch build can never ship under the universal name.
verify_universal() {
  local app="$1" exe bin archs arch
  exe="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleExecutable' "$app/Contents/Info.plist")" \
    || die "can't read CFBundleExecutable from $app"
  bin="$app/Contents/MacOS/$exe"
  archs="$(lipo -archs "$bin")" || die "lipo failed on $bin"
  for arch in arm64 x86_64; do
    [[ " $archs " == *" $arch "* ]] || die "$bin lacks the $arch slice (has: $archs)"
  done
  echo "  $exe: $archs ✓"
}

build_macos() {
  source_env

  command -v rustup >/dev/null || die "rustup not found"
  require_rust_targets $MAC_TRIPLES

  [[ -n "${APPLE_SIGNING_IDENTITY:-}" ]] || {
    echo "error: APPLE_SIGNING_IDENTITY is not set." >&2
    echo "       Copy .env.release.example to .env.release and fill it in." >&2
    exit 1
  }

  if ! security find-identity -v -p codesigning | grep -qF "$APPLE_SIGNING_IDENTITY"; then
    die "signing identity not found in keychain: $APPLE_SIGNING_IDENTITY"
  fi

  case "$APPLE_SIGNING_IDENTITY" in
    "Developer ID Application:"*) ;;
    *)
      echo "warning: '$APPLE_SIGNING_IDENTITY' is not a 'Developer ID Application'" >&2
      echo "         identity; the resulting app may not run on other Macs." >&2
      ;;
  esac

  # Tauri auto-notarizes when these are present at build time. Warn if absent so
  # a silently un-notarized build doesn't slip out.
  if [[ -z "${APPLE_API_KEY:-}" && -z "${APPLE_PASSWORD:-}" ]]; then
    echo "warning: no notarization credentials set — build will be signed but NOT" >&2
    echo "         notarized, and Gatekeeper will block it on other machines." >&2
  fi

  echo "Building signed release as: $APPLE_SIGNING_IDENTITY"
  # Marks the binary as an official release build (backend/build.rs stamps it in).
  # Without it the About panel labels the version "X.Y.Z+dev".
  MAIESTRO_RELEASE=1 pnpm tauri build --target "$MAC_TARGET"

  local app="backend/target/$MAC_TARGET/release/bundle/macos/mAIestro Code.app"
  echo
  echo "Verifying architectures…"
  verify_universal "$app"
  echo
  echo "Verifying signature…"
  codesign --verify --deep --strict --verbose=2 "$app"
  echo "Designated requirement:"
  codesign -d -r- "$app" 2>&1 | sed -n 's/^designated => /  /p'

  echo
  echo "Gatekeeper assessment:"
  spctl --assess --type execute --verbose=4 "$app" || true

  if xcrun stapler validate "$app" >/dev/null 2>&1; then
    echo "App notarization ticket: stapled ✓"
  else
    echo "App notarization ticket: not stapled"
  fi

  # The .dmg needs its own notarization ticket — tauri only staples the .app
  # inside it. publish enforces `stapler validate <dmg>`, so do it here.
  local version dmg
  version="$(read_version)"
  dmg="$(asset_local_file macos "$version")"
  echo
  if [[ -n "$dmg" ]]; then
    notarize_and_staple "$dmg" || true
    if xcrun stapler validate "$dmg" >/dev/null 2>&1; then
      echo "DMG notarization ticket: stapled ✓"
    else
      echo "DMG notarization ticket: not stapled — 'publish' will refuse it"
    fi
  else
    echo "warning: no .dmg found for $version to notarize" >&2
  fi

  echo
  echo "Done. Bundle: $app"
}

# The Windows installer, x64 only (ARM64 is #232). Unsigned until #224, which
# adds a `--config` override carrying bundle.windows.signCommand to this
# invocation.
build_windows() {
  command -v pnpm >/dev/null || die "pnpm not found"
  command -v rustup >/dev/null || die "rustup not found"
  require_rust_targets $WIN_TRIPLES

  local triple
  for triple in $WIN_TRIPLES; do
    echo "Building $triple installer (unsigned)…"
    # MAIESTRO_RELEASE: see build_macos.
    if ! MAIESTRO_RELEASE=1 pnpm tauri build --target "$triple" --bundles nsis; then
      die "building the $triple installer failed"
    fi
  done

  local version key file
  version="$(read_version)"
  echo
  for key in $(platform_asset_keys windows); do
    file="$(asset_local_file "$key" "$version")"
    [[ -n "$file" ]] || die "no $key installer for $version found at: $(asset_local_glob "$key" "$version")"
    verify_asset "$key" "$file"
    echo "$key: $file ($(wc -c <"$file" | tr -d ' ') bytes, unsigned)"
  done
  echo
  echo "Done. Next: scripts/release.sh publish --platform windows"
}

# --- GitHub API -------------------------------------------------------------

API="https://api.github.com"
OWNER="" REPO=""

# Derive owner/repo from the origin remote (https or ssh form).
resolve_repo() {
  local origin owner_repo
  origin="$(git remote get-url origin)"
  owner_repo="$(printf '%s' "$origin" | sed -E 's#^git@github\.com:##; s#^https://github\.com/##; s#\.git$##')"
  OWNER="${owner_repo%%/*}"
  REPO="${owner_repo##*/}"
  [[ "$OWNER" != "$owner_repo" && -n "$OWNER" && -n "$REPO" ]] \
    || die "could not parse owner/repo from origin: $origin"
}

# Prints the response body then a trailing line with the HTTP status code.
# body_file "-" reads the body from stdin (JSON payloads go that way rather than
# through a temp file, whose /tmp path a Windows curl wouldn't understand).
github_request() {
  local method="$1" url="$2" body_file="${3:-}" ctype="${4:-application/json}"
  local args=(-sS -X "$method"
    -H "Authorization: Bearer $GITHUB_TOKEN"
    -H "Accept: application/vnd.github+json"
    -H "X-GitHub-Api-Version: 2022-11-28"
    -w $'\n%{http_code}')
  if [[ -n "$body_file" ]]; then
    args+=(-H "Content-Type: $ctype" --data-binary @"$body_file")
  fi
  curl "${args[@]}" "$url"
}

# github_call METHOD PATH WANT_STATUS [body_file [ctype]] — prints the body,
# dies on any other status.
github_call() {
  local method="$1" path="$2" want="$3" resp status data
  resp="$(github_request "$method" "$API/repos/$OWNER/$REPO$path" "${@:4}")"
  status="${resp##*$'\n'}"; data="${resp%$'\n'*}"
  [[ "$status" == "$want" ]] || die "$method $path failed (HTTP $status): $data"
  printf '%s' "$data"
}

# Find the release for $1 (a tag), setting RELEASE_ID / UPLOAD_URL / IS_DRAFT
# (all empty if none). Drafts are invisible to /releases/tags/<tag> (it only
# resolves published releases), so this lists releases and matches tag_name
# itself. A published one wins; among drafts the oldest (lowest id) wins — the
# same rule on both machines, which is what makes the race guard work.
RELEASE_ID="" UPLOAD_URL="" IS_DRAFT=""
find_release() {
  local list found
  list="$(github_call GET "/releases?per_page=100" 200)"
  found="$(printf '%s' "$list" | TAG="$1" pyrun -c '
import json, os, sys
tag = os.environ["TAG"]
rs = [r for r in json.load(sys.stdin) if r["tag_name"] == tag]
rs.sort(key=lambda r: (r["draft"], r["id"]))
if rs:
    r = rs[0]
    print("\t".join([str(r["id"]), r["upload_url"], "1" if r["draft"] else "0"]))
')"
  RELEASE_ID="$(printf '%s' "$found" | cut -f1)"
  UPLOAD_URL="$(printf '%s' "$found" | cut -f2)"
  IS_DRAFT="$(printf '%s' "$found" | cut -f3)"
}

# Print the asset keys (from $ALL_ASSET_KEYS, or $2…) that release $1 lacks in
# state "uploaded". A failed upload (e.g. a transient HTTP 500) leaves an asset
# record behind in state "starter" — no data, never in the download list, but
# still listed — so only "uploaded" counts.
missing_assets() {
  local release_id="$1"; shift
  local keys="${*:-$ALL_ASSET_KEYS}" version key patterns="" assets
  version="$(read_version)"
  for key in $keys; do
    patterns+="$key"$'\t'"$(asset_remote_pattern "$key" "$version")"$'\n'
  done
  assets="$(github_call GET "/releases/$release_id/assets?per_page=100" 200)"
  printf '%s' "$assets" | PATTERNS="$patterns" pyrun -c '
import fnmatch, json, os, sys
names = [a["name"] for a in json.load(sys.stdin) if a.get("state") == "uploaded"]
for line in os.environ["PATTERNS"].splitlines():
    key, pat = line.split("\t")
    if not any(fnmatch.fnmatchcase(n, pat) for n in names):
        print(key)
'
}

# Upload one built file to the draft, unless a same-named asset is already
# there in state "uploaded". Any other same-named record is a corpse from a
# failed upload: delete it (allowed — the release is still a draft) first.
upload_asset() {
  local key="$1" file="$2" name assets existing existing_id existing_state
  name="$(asset_remote_name "$key" "$file")"
  assets="$(github_call GET "/releases/$RELEASE_ID/assets?per_page=100" 200)"
  existing="$(printf '%s' "$assets" | NAME="$name" pyrun -c '
import json, os, sys
for a in json.load(sys.stdin):
    if a["name"] == os.environ["NAME"]:
        print(a["id"], a.get("state", ""))
        break
')"
  existing_id="${existing%% *}"; existing_state="${existing#* }"

  if [[ "$existing_state" == "uploaded" ]]; then
    echo "Asset $name already uploaded ✓"
    return 0
  fi
  if [[ -n "$existing_id" ]]; then
    echo "Removing incomplete asset $name (state: ${existing_state:-unknown})"
    github_call DELETE "/releases/assets/$existing_id" 204 >/dev/null
  fi
  # upload_url is templated: ".../assets{?name,label}" — strip the template.
  local upload_base="${UPLOAD_URL%%\{*}" resp status data
  echo "Uploading $name"
  resp="$(github_request POST "$upload_base?name=$name" "$file" "$(asset_content_type "$key")")"
  status="${resp##*$'\n'}"; data="${resp%$'\n'*}"
  if [[ "$status" != "201" ]]; then
    # 422 here almost always means the release was published before the asset
    # was attached, and the repo has immutable releases on — nothing can be
    # added to it now. The fix is a fresh version, not a retry.
    if [[ "$status" == "422" ]]; then
      printf '%s\n' \
        "Note: the release looks already-published and immutable — assets can" \
        "      no longer be attached to it. Cut the next patch version instead." >&2
    fi
    die "uploading $name failed (HTTP $status): $data"
  fi
  echo "Uploaded $name ✓"
}

print_release_url() {
  local release url
  release="$(github_call GET "/releases/$RELEASE_ID" 200)"
  url="$(printf '%s' "$release" | pyrun -c 'import json,sys;print(json.load(sys.stdin)["html_url"])')"
  echo
  echo "$1: $url"
}

# --- publish ----------------------------------------------------------------

cmd_publish() {
  local platform notes_file="" prerelease=false
  platform="$(host_platform)"
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --platform)   platform="${2:-}"; shift 2 ;;
      --notes-file) notes_file="${2:-}"; shift 2 ;;
      --prerelease) prerelease=true; shift ;;
      *) die "unknown publish arg: $1" ;;
    esac
  done
  check_platform "$platform"
  [[ -z "$notes_file" || -f "$notes_file" ]] || die "notes file not found: $notes_file"

  source_env
  [[ -n "${GITHUB_TOKEN:-}" ]] || die "GITHUB_TOKEN is not set (add it to .env.release)"

  command -v git >/dev/null || die "git not found"
  [[ -z "$(git status --porcelain)" ]] || die "working tree is dirty — commit the release before publishing"

  local version tag
  version="$(read_version)"
  tag="v$version"
  resolve_repo

  # 0. Locate this platform's built assets and check they're fit to ship.
  local key file keys files=()
  keys="$(platform_asset_keys "$platform")"
  for key in $keys; do
    file="$(asset_local_file "$key" "$version")"
    [[ -n "$file" ]] || die "no $key asset for $version — run 'scripts/release.sh build --platform $platform' first"
    verify_asset "$key" "$file"
    files+=("$file")
  done

  # 1. Tag. The tag is what ties the two machines to one commit: if the other
  # machine already pushed it, fetch it and refuse unless it is our HEAD, so
  # they can never ship different commits under one version.
  local head_sha; head_sha="$(git rev-parse HEAD)"
  git fetch -q origin "refs/tags/$tag:refs/tags/$tag" 2>/dev/null || true
  if git rev-parse -q --verify "refs/tags/$tag" >/dev/null; then
    local tag_sha; tag_sha="$(git rev-parse "$tag^{commit}")"
    [[ "$tag_sha" == "$head_sha" ]] \
      || die "tag $tag already exists at $tag_sha, not HEAD ($head_sha) — check out that commit to release $platform"
    echo "Tag $tag already at HEAD ✓"
  else
    echo "Creating tag $tag"
    git tag "$tag"
  fi
  # Push the tag (no-op if the remote already has it at this sha).
  git push origin "$tag"

  # 2. Release. Reuse the one for the tag (whichever machine created it), else
  # create it as a *draft*. It is only published by finalize_release, once
  # every platform's assets are attached: with the repo's immutable releases
  # setting on, publishing freezes the release *and its assets*, so anything not
  # yet attached could never be added — the upload comes back "Cannot upload
  # assets to an immutable release" (HTTP 422). A later run never touches the
  # notes the first run set.
  find_release "$tag"
  if [[ -n "$RELEASE_ID" && "$IS_DRAFT" == "0" ]]; then
    # Already public. Fine if it already carries our assets (a re-run).
    local ours_missing
    # shellcheck disable=SC2086
    ours_missing="$(missing_assets "$RELEASE_ID" $keys)"
    if [[ -z "$ours_missing" ]]; then
      echo "Release $tag is already published with the $platform assets ✓"
      print_release_url "Released $tag"
      return 0
    fi
    die "release $tag is already published without the $platform assets, and immutable — cut the next patch version"
  elif [[ -n "$RELEASE_ID" ]]; then
    echo "Draft release $tag already exists — reusing"
  else
    echo "Creating draft release $tag"
    local payload created mine
    payload="$(NOTES_FILE="$notes_file" TAG="$tag" SHA="$head_sha" PRE="$prerelease" pyrun -c '
import json, os
notes_file = os.environ.get("NOTES_FILE") or ""
body = open(notes_file).read() if notes_file else ""
print(json.dumps({
    "tag_name": os.environ["TAG"], "name": os.environ["TAG"], "body": body,
    "target_commitish": os.environ["SHA"],
    "draft": True, "prerelease": os.environ["PRE"] == "true",
}))
')"
    created="$(printf '%s' "$payload" | github_call POST "/releases" 201 -)"
    mine="$(printf '%s' "$created" | pyrun -c 'import json,sys;print(json.load(sys.stdin)["id"])')"
    # Race guard: if the other machine created a draft at the same moment, both
    # re-list and keep the oldest; the loser deletes its own. The listing lags
    # creation (it is served with max-age=60), so a fresh draft can be missing
    # from it for up to a minute: re-list until ours (or an older one) shows
    # up, and never read an empty or newer result as having lost.
    local tries=0
    find_release "$tag"
    while { [[ -z "$RELEASE_ID" ]] || (( RELEASE_ID > mine )); } && (( tries < 40 )); do
      sleep 2
      tries=$((tries + 1))
      find_release "$tag"
    done
    if [[ -z "$RELEASE_ID" ]] || (( RELEASE_ID > mine )); then
      RELEASE_ID="$mine"
      UPLOAD_URL="$(printf '%s' "$created" | pyrun -c 'import json,sys;print(json.load(sys.stdin)["upload_url"])')"
      IS_DRAFT=1
    elif [[ "$RELEASE_ID" != "$mine" ]]; then
      echo "Another draft for $tag won the race — using it, deleting ours ($mine)"
      github_call DELETE "/releases/$mine" 204 >/dev/null
    fi
  fi

  # 3. Assets.
  local i=0
  for key in $keys; do
    upload_asset "$key" "${files[$i]}"
    i=$((i + 1))
  done

  # 4. Publish if that completed the set.
  finalize_release "$tag" ""
}

# --- finalize ---------------------------------------------------------------

cmd_finalize() {
  local allow="" k
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --allow-missing)
        k="${2:-}"
        [[ " $ALL_ASSET_KEYS " == *" $k "* ]] || die "unknown asset key: '$k' (want one of: $ALL_ASSET_KEYS)"
        allow+="$k "; shift 2 ;;
      *) die "unknown finalize arg: $1" ;;
    esac
  done

  source_env
  [[ -n "${GITHUB_TOKEN:-}" ]] || die "GITHUB_TOKEN is not set (add it to .env.release)"
  resolve_repo
  local tag; tag="v$(read_version)"
  find_release "$tag"
  [[ -n "$RELEASE_ID" ]] || die "no release for $tag yet — run 'scripts/release.sh publish' first"
  if [[ "$IS_DRAFT" == "0" ]]; then
    echo "Release $tag is already published ✓"
    print_release_url "Released $tag"
    return 0
  fi
  finalize_release "$tag" "$allow"
}

# Publish the draft $RELEASE_ID if every expected asset is attached, or if the
# only missing ones are in $2 (space-separated keys, then named in the notes).
# Otherwise print what's missing and leave it a draft.
#
# The asset check re-reads the API rather than trusting the uploads: publishing
# is the irreversible step — immutability freezes whatever is attached at that
# moment, so an incomplete release can never be repaired, only deleted and re-cut.
finalize_release() {
  local tag="$1" allow="$2" missing key unallowed=""
  missing="$(missing_assets "$RELEASE_ID" | tr '\n' ' ')"
  missing="${missing% }"
  for key in $missing; do
    [[ " $allow " == *" $key "* ]] || unallowed+="$key "
  done
  if [[ -n "$unallowed" ]]; then
    echo
    echo "Draft $tag is still missing: ${unallowed% } — left as a draft."
    echo "Run 'scripts/release.sh publish' on the machine that builds it (same commit),"
    echo "or 'scripts/release.sh finalize --allow-missing <key>' to ship without it."
    print_release_url "Draft $tag"
    return 0
  fi

  local payload='{"draft": false}' release
  if [[ -n "$missing" ]]; then
    echo "Publishing without: $missing"
    release="$(github_call GET "/releases/$RELEASE_ID" 200)"
    payload="$(printf '%s' "$release" | MISSING="$missing" pyrun -c '
import json, os, sys
r = json.load(sys.stdin)
keys = ", ".join("`%s`" % k for k in os.environ["MISSING"].split())
body = (r.get("body") or "").rstrip()
body += ("\n\n" if body else "") + "_Not included in this release: %s._" % keys
print(json.dumps({"draft": False, "body": body}))
')"
  fi
  echo "Publishing release $tag"
  printf '%s' "$payload" | github_call PATCH "/releases/$RELEASE_ID" 200 - >/dev/null
  echo "Published $tag ✓"
  print_release_url "Released $tag"
}

# --- dispatch ---------------------------------------------------------------

resolve_python

cmd="${1:-}"
[[ $# -gt 0 ]] && shift || true
case "$cmd" in
  bump)     cmd_bump "$@" ;;
  build)    cmd_build "$@" ;;
  publish)  cmd_publish "$@" ;;
  finalize) cmd_finalize "$@" ;;
  ""|-h|--help|help) usage; [[ "$cmd" == "" ]] && exit 1 || exit 0 ;;
  *) echo "error: unknown command: $cmd" >&2; usage; exit 1 ;;
esac
