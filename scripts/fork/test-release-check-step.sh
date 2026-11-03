#!/usr/bin/env bash
# herdr-fork: fork-only. Runs the real "Check the binary's architecture and that it runs" step of
# .github/workflows/fork-release.yml under /bin/bash against a stub binary. On macOS /bin/bash is
# 3.2, where expanding an empty array under `set -u` is an error; bash 5 on Linux is not, so only
# a macOS runner (or a Mac) can catch that. Unofficial stopgap; see FORK.md.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../.." && pwd)
tmp=$(mktemp -d)
trap 'rm -r "$tmp"' EXIT
fail=0

# The step text comes from the workflow itself, read with the policy test's YAML reader.
python3 -I - "$here/test-fork-release-workflow.py" "$tmp/step.sh" <<'PY'
import importlib.util
import sys

spec = importlib.util.spec_from_file_location("policy", sys.argv[1])
policy = importlib.util.module_from_spec(spec)
spec.loader.exec_module(policy)
_, wf = policy.load_workflow(policy.WORKFLOW_PATH)
step = policy.step_named(wf["jobs"]["build"], "Check the binary's architecture and that it runs")
with open(sys.argv[2], "w", encoding="utf-8") as fh:
    fh.write(step["run"])
PY

# Stubs: `file` prints the line the case expects, `arch` drops its first argument and runs the rest.
stubs=$tmp/stubs
mkdir -p "$stubs"
cat >"$stubs/file" <<'EOF'
#!/bin/sh
printf '%s: %s\n' "$1" "$STUB_FILE_LINE"
EOF
cat >"$stubs/arch" <<'EOF'
#!/bin/sh
shift
exec "$@"
EOF
chmod +x "$stubs/file" "$stubs/arch"

# run_case <name> <want: pass|fail> <run_prefix> <file_expect> <file_line> <version_output>
run_case() {
  local name=$1 want=$2 prefix=$3 expect=$4 line=$5 version=$6
  local work=$tmp/work-$name
  mkdir -p "$work/target/aarch64-apple-darwin/release"
  printf '#!/bin/sh\necho "%s"\n' "$version" >"$work/target/aarch64-apple-darwin/release/herdr"
  chmod +x "$work/target/aarch64-apple-darwin/release/herdr"
  local rc=0
  (
    cd "$work"
    env -i HOME="$tmp" "PATH=$stubs:/usr/bin:/bin" STUB_FILE_LINE="$line" \
      TAG=v0.9.5-ev.2 TARGET=aarch64-apple-darwin RUN_PREFIX="$prefix" FILE_EXPECT="$expect" \
      /bin/bash "$tmp/step.sh"
  ) >"$tmp/out-$name" 2>&1 || rc=$?
  if { [ "$want" = pass ] && [ "$rc" -eq 0 ]; } || { [ "$want" = fail ] && [ "$rc" -ne 0 ]; }; then
    echo "ok   $name (rc=$rc)"
  else
    echo "FAIL $name: wanted $want, got rc=$rc"
    sed 's/^/     | /' "$tmp/out-$name"
    fail=1
  fi
}

good='herdr 0.9.5 (fork v0.9.5-ev.2)'
arm='Mach-O 64-bit executable arm64'
x86='Mach-O 64-bit executable x86_64'

echo "bash: $(/bin/bash --version | head -n 1)"
run_case empty-prefix pass "" "$arm" "$arm" "$good"
run_case arch-prefix pass "arch -x86_64" "$x86" "$x86" "$good"
run_case wrong-version fail "arch -x86_64" "$x86" "$x86" "herdr 0.9.4 (fork v0.9.4-ev.1)"

[ "$fail" -eq 0 ] || exit 1
echo "release check step: all cases passed"
