#!/usr/bin/env bash
#
# Release pipeline for mAIestro Code. Three phases, each independently runnable and
# idempotent so a partially-failed release can be resumed by re-running it:
#
#   release.sh bump <patch|minor|major|X.Y.Z>
#       Bump the version in backend/tauri.conf.json, package.json,
#       backend/Cargo.toml, and backend/Cargo.lock, then assert they agree.
#       Does NOT commit — the caller commits.
#
#   release.sh build
#       Source .env.release, validate the signing identity, `tauri build`, and
#       verify the signature / Gatekeeper assessment / notarization staple.
#       tauri only notarizes the .app, so this also submits + staples the .dmg
#       (which publish requires); already-stapled artifacts are skipped.
#
#   release.sh publish [--notes-file <file>]
#       Tag vX.Y.Z, create the GitHub Release (REST API, not `gh`), and upload
#       the signed+notarized .dmg. Each step is skipped if already done.
#
# Why signing matters here (beyond distribution): macOS binds a Keychain item's
# "Always Allow" decision to the app's *designated requirement*. For an ad-hoc /
# unsigned build that requirement is just the binary's cdhash, which changes on
# every rebuild — so the access prompt comes back every launch. A stable
# Developer ID signature anchors the requirement to the certificate instead, so
# "Always Allow" persists across releases.
#
# Requirements:
#   * A "Developer ID Application" certificate in the login keychain. NOT an
#     "Apple Development" cert — that one is for local testing only and the app
#     will not launch on other people's Macs.
#   * Notarization credentials (see below). Without them the build is signed but
#     not notarized, and Gatekeeper will block it on other machines.
#   * For `publish`: a GITHUB_TOKEN with contents:write on this repo.
#
# Configuration lives in a gitignored .env.release at the repo root (see
# .env.release.example). The script sources it, so you never export by hand.
#
#   APPLE_SIGNING_IDENTITY   e.g. "Developer ID Application: Your Name (CCMY5ZR77Q)"
#   GITHUB_TOKEN             fine-grained PAT, contents:write on this repo
#
# Notarization — provide EITHER an App Store Connect API key:
#   APPLE_API_ISSUER, APPLE_API_KEY, APPLE_API_KEY_PATH
# OR an Apple ID app-specific password:
#   APPLE_ID, APPLE_PASSWORD, APPLE_TEAM_ID
#
# List available identities with:  security find-identity -v -p codesigning

set -euo pipefail
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

# The version is single-sourced from tauri.conf.json.
read_version() {
  python3 -c 'import json;print(json.load(open("backend/tauri.conf.json"))["version"])'
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

  bump <patch|minor|major|X.Y.Z>   bump version across all manifests
  build                            build, sign, notarize, and verify the .app
  publish [--notes-file <file>]    tag + create GitHub Release + upload .dmg

Each command is idempotent; re-run to resume a partial release.
EOF
}

# --- bump -------------------------------------------------------------------

cmd_bump() {
  local spec="${1:-}"
  [[ -n "$spec" ]] || die "bump needs a spec: patch | minor | major | X.Y.Z"

  local current new
  current="$(read_version)"
  new="$(python3 - "$current" "$spec" <<'PY'
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
  python3 - "$new" <<'PY'
import re, sys
new = sys.argv[1]
edits = [
    ("backend/tauri.conf.json", r'("version"\s*:\s*")[^"]*(")', rf'\g<1>{new}\g<2>'),
    ("package.json",            r'("version"\s*:\s*")[^"]*(")', rf'\g<1>{new}\g<2>'),
    ("backend/Cargo.toml",      r'(?m)^version = "[^"]*"',       f'version = "{new}"'),
    ("backend/Cargo.lock",      r'(?m)(^name = "maiestro"\nversion = ")[^"]*(")',
                                rf'\g<1>{new}\g<2>'),
]
for path, pat, repl in edits:
    s = open(path).read()
    s2, n = re.subn(pat, repl, s, count=1)
    if n != 1:
        sys.exit(f"error: expected exactly one version field in {path}, found {n}")
    open(path, "w").write(s2)
PY

  # Assert every manifest (and the lockfile) now agree.
  python3 - "$new" <<'PY'
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
  echo "Bumped to $new. Review, commit, then: scripts/release.sh build && scripts/release.sh publish"
}

# --- build ------------------------------------------------------------------

cmd_build() {
  source_env

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
  # Without it the About panel labels the version "X.Y.Z+dev" — see #126.
  MAIESTRO_RELEASE=1 pnpm tauri build

  local app="backend/target/release/bundle/macos/mAIestro Code.app"
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
  local version dmg dmgs
  version="$(read_version)"
  dmgs=(backend/target/release/bundle/dmg/"mAIestro Code_${version}_"*.dmg)
  dmg="${dmgs[0]}"
  echo
  if [[ -f "$dmg" ]]; then
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

# --- publish ----------------------------------------------------------------

# Prints the response body then a trailing line with the HTTP status code.
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

cmd_publish() {
  local notes_file=""
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --notes-file) notes_file="${2:-}"; shift 2 ;;
      *) die "unknown publish arg: $1" ;;
    esac
  done
  [[ -z "$notes_file" || -f "$notes_file" ]] || die "notes file not found: $notes_file"

  source_env
  [[ -n "${GITHUB_TOKEN:-}" ]] || die "GITHUB_TOKEN is not set (add it to .env.release)"

  command -v git >/dev/null || die "git not found"
  [[ -z "$(git status --porcelain)" ]] || die "working tree is dirty — commit the release before publishing"

  local version tag
  version="$(read_version)"
  tag="v$version"

  # Derive owner/repo from the origin remote (https or ssh form).
  local origin owner_repo owner repo
  origin="$(git remote get-url origin)"
  owner_repo="$(printf '%s' "$origin" | sed -E 's#^git@github\.com:##; s#^https://github\.com/##; s#\.git$##')"
  owner="${owner_repo%%/*}"
  repo="${owner_repo##*/}"
  [[ "$owner" != "$owner_repo" && -n "$owner" && -n "$repo" ]] \
    || die "could not parse owner/repo from origin: $origin"

  # Locate the signed dmg and confirm it is notarized before publishing.
  local dmg dmgs=(backend/target/release/bundle/dmg/"mAIestro Code_${version}_"*.dmg)
  dmg="${dmgs[0]}"
  [[ -f "$dmg" ]] || die "no .dmg for $version — run 'scripts/release.sh build' first"
  xcrun stapler validate "$dmg" >/dev/null 2>&1 \
    || die "$dmg is not notarized (stapler validate failed) — refusing to publish"

  local api="https://api.github.com"
  local resp status data

  # 1. Tag. Reuse if it already points at HEAD; conflict if it's elsewhere.
  local head_sha; head_sha="$(git rev-parse HEAD)"
  if git rev-parse -q --verify "refs/tags/$tag" >/dev/null; then
    local tag_sha; tag_sha="$(git rev-parse "$tag^{commit}")"
    [[ "$tag_sha" == "$head_sha" ]] \
      || die "tag $tag already exists at $tag_sha, not HEAD ($head_sha)"
    echo "Tag $tag already at HEAD ✓"
  else
    echo "Creating tag $tag"
    git tag "$tag"
  fi
  # Push the tag (no-op if the remote already has it at this sha).
  git push origin "$tag"

  # 2. Release. Reuse an existing one for the tag, else create it as a *draft*.
  #
  # It is created as a draft and only published in step 4, once the .dmg is
  # attached. With the repo's immutable releases setting on, publishing freezes
  # the release *and its assets*, so a release created already-published can
  # never receive its .dmg — the upload comes back "Cannot upload assets to an
  # immutable release" (HTTP 422). A draft is still mutable, so the only order
  # that works is create-draft -> upload -> publish.
  #
  # Drafts are invisible to /releases/tags/<tag> (it only resolves published
  # releases), so the reuse lookup lists releases and matches tag_name itself —
  # otherwise re-running after a failed upload would create a second release
  # instead of resuming the draft.
  resp="$(github_request GET "$api/repos/$owner/$repo/releases?per_page=100")"
  status="${resp##*$'\n'}"; data="${resp%$'\n'*}"
  [[ "$status" == "200" ]] || die "listing releases failed (HTTP $status): $data"

  local release_id upload_url is_draft found
  found="$(printf '%s' "$data" | TAG="$tag" python3 -c '
import json, os, sys
tag = os.environ["TAG"]
for r in json.load(sys.stdin):
    if r["tag_name"] == tag:
        print("\t".join([str(r["id"]), r["upload_url"], "1" if r["draft"] else "0"]))
        break
')"
  if [[ -n "$found" ]]; then
    echo "Release $tag already exists — reusing"
    release_id="$(printf '%s' "$found" | cut -f1)"
    upload_url="$(printf '%s' "$found" | cut -f2)"
    is_draft="$(printf '%s' "$found" | cut -f3)"
  else
    echo "Creating draft release $tag"
    local body_json
    body_json="$(NOTES_FILE="$notes_file" TAG="$tag" python3 <<'PY'
import json, os
notes_file = os.environ.get("NOTES_FILE") or ""
tag = os.environ["TAG"]
body = open(notes_file).read() if notes_file else ""
print(json.dumps({
    "tag_name": tag, "name": tag, "body": body,
    "draft": True, "prerelease": False,
}))
PY
)"
    local tmp_payload; tmp_payload="$(mktemp)"
    printf '%s' "$body_json" > "$tmp_payload"
    resp="$(github_request POST "$api/repos/$owner/$repo/releases" "$tmp_payload")"
    rm -f "$tmp_payload"
    status="${resp##*$'\n'}"; data="${resp%$'\n'*}"
    [[ "$status" == "201" ]] || die "creating release failed (HTTP $status): $data"
    release_id="$(printf '%s' "$data" | python3 -c 'import json,sys;print(json.load(sys.stdin)["id"])')"
    upload_url="$(printf '%s' "$data" | python3 -c 'import json,sys;print(json.load(sys.stdin)["upload_url"])')"
    is_draft=1
  fi

  # 3. Asset. Skip if a same-named asset is already attached.
  # The bundled file is "mAIestro Code_<ver>_<arch>.dmg"; the space isn't
  # URL-safe and GitHub would rename it anyway, so upload it hyphenated.
  local dmg_name; dmg_name="$(basename "$dmg")"; dmg_name="${dmg_name// /-}"
  resp="$(github_request GET "$api/repos/$owner/$repo/releases/$release_id/assets")"
  status="${resp##*$'\n'}"; data="${resp%$'\n'*}"
  [[ "$status" == "200" ]] || die "listing assets failed (HTTP $status): $data"

  # Match on name *and* state. A failed upload (e.g. a transient HTTP 500)
  # leaves the asset record behind in state "starter" — it holds no data and
  # never appears in the release's download list, but it is still returned
  # here. Treating that placeholder as "already uploaded" would skip the real
  # upload and then publish an assetless release, which immutability makes
  # permanent. So only state "uploaded" counts; any other state is a corpse to
  # delete (the release is still a draft here, so deleting is allowed) before
  # re-uploading under the same name.
  local existing_id existing_state
  existing_state="$(printf '%s' "$data" | NAME="$dmg_name" python3 -c '
import json, os, sys
name = os.environ["NAME"]
for a in json.load(sys.stdin):
    if a["name"] == name:
        print(a["id"], a.get("state", ""))
        break
')"
  existing_id="${existing_state%% *}"; existing_state="${existing_state#* }"

  if [[ "$existing_state" == "uploaded" ]]; then
    echo "Asset $dmg_name already uploaded ✓"
  else
    if [[ -n "$existing_id" ]]; then
      echo "Removing incomplete asset $dmg_name (state: ${existing_state:-unknown})"
      resp="$(github_request DELETE "$api/repos/$owner/$repo/releases/assets/$existing_id")"
      status="${resp##*$'\n'}"; data="${resp%$'\n'*}"
      [[ "$status" == "204" ]] || die "removing stale asset $dmg_name failed (HTTP $status): $data"
    fi
    # upload_url is templated: ".../assets{?name,label}" — strip the template.
    local upload_base="${upload_url%%\{*}"
    echo "Uploading $dmg_name"
    resp="$(github_request POST "$upload_base?name=$dmg_name" "$dmg" "application/x-apple-diskimage")"
    status="${resp##*$'\n'}"; data="${resp%$'\n'*}"
    if [[ "$status" != "201" ]]; then
      # 422 here almost always means the release was published before the asset
      # was attached, and the repo has immutable releases on — nothing can be
      # added to it now. The fix is a fresh version, not a retry.
      if [[ "$status" == "422" ]]; then
        printf '%s\n' \
          "Note: release $tag looks already-published and immutable — assets can" \
          "      no longer be attached to it. Cut the next patch version instead." >&2
      fi
      die "uploading $dmg_name failed (HTTP $status): $data"
    fi
    echo "Uploaded $dmg_name ✓"
  fi

  # 4. Publish. The draft becomes a real release only now, with the .dmg already
  # attached — see the immutable-releases note in step 2.
  #
  # Re-read the assets and refuse to publish unless the .dmg is really there in
  # state "uploaded". Publishing is the irreversible step: immutability freezes
  # whatever is attached at that moment, so an assetless release can never be
  # repaired, only deleted and re-cut. Verify against the API rather than trust
  # that step 3 did its job.
  if [[ "$is_draft" == "1" ]]; then
    resp="$(github_request GET "$api/repos/$owner/$repo/releases/$release_id/assets")"
    status="${resp##*$'\n'}"; data="${resp%$'\n'*}"
    [[ "$status" == "200" ]] || die "re-checking assets failed (HTTP $status): $data"
    printf '%s' "$data" | NAME="$dmg_name" python3 -c '
import json, os, sys
name = os.environ["NAME"]
sys.exit(0 if any(a["name"] == name and a.get("state") == "uploaded"
                  for a in json.load(sys.stdin)) else 1)' \
      || die "refusing to publish $tag: $dmg_name is not attached in state 'uploaded'." \
             $'\n''Publishing now would freeze an assetless release permanently.'

    echo "Publishing release $tag"
    local tmp_pub; tmp_pub="$(mktemp)"
    printf '%s' '{"draft": false}' > "$tmp_pub"
    resp="$(github_request PATCH "$api/repos/$owner/$repo/releases/$release_id" "$tmp_pub")"
    rm -f "$tmp_pub"
    status="${resp##*$'\n'}"; data="${resp%$'\n'*}"
    [[ "$status" == "200" ]] || die "publishing release $tag failed (HTTP $status): $data"
    echo "Published $tag ✓"
  fi

  local html_url; html_url="$(github_request GET "$api/repos/$owner/$repo/releases/$release_id" \
    | sed '$d' | python3 -c 'import json,sys;print(json.load(sys.stdin)["html_url"])')"
  echo
  echo "Released $tag: $html_url"
}

# --- dispatch ---------------------------------------------------------------

cmd="${1:-}"
[[ $# -gt 0 ]] && shift || true
case "$cmd" in
  bump)    cmd_bump "$@" ;;
  build)   cmd_build "$@" ;;
  publish) cmd_publish "$@" ;;
  ""|-h|--help|help) usage; [[ "$cmd" == "" ]] && exit 1 || exit 0 ;;
  *) echo "error: unknown command: $cmd" >&2; usage; exit 1 ;;
esac
