#!/usr/bin/env bash
# herdr-fork: fork-only. Tests scripts/fork/run-mutants.sh against a fixture repo with a stub
# `cargo`, so no real build or test runs. Unofficial stopgap; see FORK.md and docs/ci.md.
#
# The stub reads the fixture's guard.txt: a build "fails" when it holds BROKEN, and a test run
# "fails" (nextest exit 100) when it holds KILL. Every call is logged with the guard content it saw,
# which shows the patch was applied where cargo ran and that the previous mutant was reverted.
set -u
here=$(cd "$(dirname "$0")" && pwd)
runner=$here/run-mutants.sh
pass=0
fail=0
check() { # name, condition result (0 = ok), detail
  if [[ $2 -eq 0 ]]; then pass=$((pass + 1)); echo "ok   $1"; else fail=$((fail + 1)); echo "FAIL $1${3:+: $3}"; fi
}

tmp=$(mktemp -d)
trap 'rm -r "$tmp"' EXIT
stubs=$tmp/stubs
mkdir -p "$stubs"
cat >"$stubs/cargo" <<'STUB'
#!/usr/bin/env bash
guard=$(sed -n 1p guard.txt)
filter=
prev=
for arg in "$@"; do
  [[ $prev == -E ]] && filter=$arg
  prev=$arg
done
case " $* " in
  *" --no-run "*)
    echo "build saw=$guard" >>"$STUB_LOG"
    [[ $guard == *BROKEN* ]] && { echo "error: could not compile"; exit 101; }
    [[ $guard == GUARD=on && ${STUB_BASE:-} == build ]] && { echo "error: baseline does not compile"; exit 101; }
    exit 0 ;;
esac
echo "run saw=$guard filter=$filter args=$*" >>"$STUB_LOG"
[[ $guard == *STRAY* ]] && touch stray.txt
[[ $guard == GUARD=on && ${STUB_BASE:-} == run ]] && { echo "        FAIL [   0.010s] fake::tests::baseline"; exit 100; }
[[ $guard == *NOTESTS* ]] && { echo "error: no tests to run"; exit 4; }
[[ $guard == *CRASH* ]] && { echo "error: test binary crashed"; exit 101; }
if [[ $guard == *STRAY* ]]; then exit 100; fi
if [[ $guard == *KILL* ]]; then
  echo "        FAIL [   0.010s] fake::tests::guard_holds"
  exit 100
fi
echo "     Summary [   0.010s] 1 test run: 1 passed"
exit 0
STUB
chmod +x "$stubs/cargo"

mkdir -p "$tmp/fx/scripts/fork/mutants"
fx=$tmp/fx
git -C "$fx" init -q
git -C "$fx" config user.email t@example.invalid
git -C "$fx" config user.name t
printf 'GUARD=on\nrest\n' >"$fx/guard.txt"
git -C "$fx" add -A
git -C "$fx" commit -qm base
mkpatch() { # name, new first line
  printf '%s\nrest\n' "$2" >"$fx/guard.txt"
  git -C "$fx" diff >"$fx/scripts/fork/mutants/$1.patch"
  git -C "$fx" checkout -q -- guard.txt
}
mkpatch kill GUARD=KILL
mkpatch weak GUARD=weak
mkpatch broken GUARD=BROKEN
mkpatch kill2 GUARD=KILL2
mkpatch notests GUARD=NOTESTS
mkpatch crash GUARD=CRASH
mkpatch stray GUARD=STRAY
# A patch written against other content: it does not apply at the base.
sed -e 's/^-GUARD=on/-GUARD=other/' "$fx/scripts/fork/mutants/kill.patch" >"$fx/scripts/fork/mutants/stale.patch"
tab=$'\t'
write_manifest() { # lines...
  {
    echo "# comment line"
    echo
    printf '%s\n' "$@"
  } >"$fx/scripts/fork/mutants/manifest.tsv"
}
git -C "$fx" add -A
git -C "$fx" commit -qm patches

run() { # shard, [extra env words...]; sets out, rc, log
  : >"$tmp/stub.log"
  out=$(MUTANTS_ROOT=$fx STUB_LOG=$tmp/stub.log PATH="$stubs:$PATH" bash "$runner" "$@" 2>&1)
  rc=$?
}
has() { [[ $out == *"$1"* ]]; }

# 1. A killed mutant gives exit 0, with the filter passed to nextest.
write_manifest "K1${tab}kill.patch${tab}test(guard_holds)"
run 1/1
check "killed mutant exits 0" "$rc" "rc=$rc"
has "K1" && has "KILLED"; check "killed mutant is reported KILLED" $?
grep -q 'run saw=GUARD=KILL filter=test(guard_holds) ' "$tmp/stub.log"; check "the filter reaches nextest, with the patch applied" $?
grep 'run saw=GUARD=KILL' "$tmp/stub.log" | grep -q -- '--no-tests=fail' && grep 'run saw=GUARD=KILL' "$tmp/stub.log" | grep -q -- '--profile mutants'; check "mutant runs pass --no-tests=fail and --profile mutants" $?

# 2. A survivor gives exit 1.
write_manifest "S1${tab}weak.patch${tab}test(guard_holds)"
run 1/1
[[ $rc -eq 1 ]]; check "surviving mutant exits 1" $? "rc=$rc"
has "SURVIVED"; check "surviving mutant is reported SURVIVED" $?

# 3. A stale patch gives exit 1 and never reaches cargo.
write_manifest "X1${tab}stale.patch${tab}test(guard_holds)"
run 1/1
[[ $rc -eq 1 ]]; check "stale patch exits 1" $? "rc=$rc"
has "STALE"; check "stale patch is reported STALE" $?
! grep -q 'saw=GUARD=\(KILL\|weak\)' "$tmp/stub.log"; check "stale patch runs no cargo on a mutated tree" $?

# 4. A missing patch file is stale too.
write_manifest "X2${tab}no-such.patch${tab}test(guard_holds)"
run 1/1
[[ $rc -eq 1 ]] && has "STALE"; check "missing patch file is STALE" $? "rc=$rc"

# 5. A broken build gives exit 1 and the tests never run.
write_manifest "B1${tab}broken.patch${tab}test(guard_holds)"
run 1/1
[[ $rc -eq 1 ]]; check "broken build exits 1" $? "rc=$rc"
has "BROKEN"; check "broken build is reported BROKEN" $?
! grep -q '^run saw=GUARD=BROKEN' "$tmp/stub.log"; check "broken build runs no tests" $?

# 6. Shard selection takes every total-th line from shard (comments and blanks are skipped).
write_manifest "M1${tab}kill.patch${tab}f1" "M2${tab}kill2.patch${tab}f2" "M3${tab}kill.patch${tab}f3" \
  "M4${tab}kill2.patch${tab}f4" "M5${tab}kill.patch${tab}f5"
ids() { grep -o '^\(M[0-9]\)[[:space:]]\+KILLED' <<<"$out" | awk '{print $1}' | sort -u | tr '\n' ' '; }
run 1/2; [[ $(ids) == "M1 M3 M5 " && $rc -eq 0 ]]; check "shard 1/2 takes M1 M3 M5" $? "got: $(ids) rc=$rc"
run 2/2; [[ $(ids) == "M2 M4 " && $rc -eq 0 ]]; check "shard 2/2 takes M2 M4" $? "got: $(ids) rc=$rc"
run 3/3; [[ $(ids) == "M3 " && $rc -eq 0 ]]; check "shard 3/3 takes M3" $? "got: $(ids) rc=$rc"
run 1/5; [[ $(ids) == "M1 " ]]; check "shard 1/5 takes M1" $? "got: $(ids)"
grep -q 'filter=f1 args' "$tmp/stub.log" && ! grep -q 'filter=f2 args' "$tmp/stub.log"; check "only the shard's filters run" $?

# 7. One failure does not hide the others, and the exit is non-zero.
write_manifest "M1${tab}kill.patch${tab}f1" "M2${tab}weak.patch${tab}f2" "M3${tab}kill2.patch${tab}f3"
run 1/1
[[ $rc -eq 1 ]] && has "M2" && has "SURVIVED" && grep -q 'filter=f3 args' "$tmp/stub.log"; check "a survivor does not stop later mutants" $? "rc=$rc"

# 8. The tree is restored after each mutant: each cargo call sees only its own mutation,
#    and the work copy ends clean and the source repo is untouched.
write_manifest "R1${tab}kill.patch${tab}f1" "R2${tab}weak.patch${tab}f2" "R3${tab}broken.patch${tab}f3" "R4${tab}kill2.patch${tab}f4"
work=$tmp/work
: >"$tmp/stub.log"
out=$(MUTANTS_ROOT=$fx MUTANTS_WORK=$work STUB_LOG=$tmp/stub.log PATH="$stubs:$PATH" bash "$runner" 1/1 2>&1)
rc=$?
saw=$(sed -e 's/ filter=.*//' "$tmp/stub.log" | tr '\n' ' ')
[[ $saw == "build saw=GUARD=on run saw=GUARD=on build saw=GUARD=KILL run saw=GUARD=KILL build saw=GUARD=weak run saw=GUARD=weak build saw=GUARD=BROKEN build saw=GUARD=KILL2 run saw=GUARD=KILL2 " ]]
check "each mutant is built on a clean tree" $? "saw: $saw"
[[ -z $(git -C "$work" status --porcelain) && $(sed -n 1p "$work/guard.txt") == GUARD=on ]]; check "work copy is restored at the end" $?
[[ -z $(git -C "$fx" status --porcelain -- guard.txt scripts/fork/mutants/kill.patch) && $(sed -n 1p "$fx/guard.txt") == GUARD=on ]]; check "source repo is never touched" $?

# 9. Usage errors exit 2.
for bad in "" 0/4 5/4 1 a/b 1/0; do
  out=$(MUTANTS_ROOT=$fx STUB_LOG=$tmp/stub.log PATH="$stubs:$PATH" bash "$runner" $bad 2>&1)
  rc=$?
  [[ $rc -eq 2 ]]; check "bad shard '$bad' exits 2" $? "rc=$rc"
done
write_manifest "Z1${tab}kill.patch"
run 1/1
[[ $rc -eq 2 ]]; check "a malformed manifest line exits 2" $? "rc=$rc"

# 10. Verdicts for a run that fails without a test failing (nextest exit 4 and 101) are ERROR.
write_manifest "N1${tab}notests.patch${tab}test(gone)"
run 1/1
[[ $rc -eq 1 ]] && has "ERROR"; check "no tests matched (exit 4) is ERROR, exit 1" $? "rc=$rc"
grep -q "^N1 *KILLED" <<<"$out" && r=1 || r=0; check "no tests matched is not KILLED" $r
write_manifest "C1${tab}crash.patch${tab}test(guard_holds)"
run 1/1
[[ $rc -eq 1 ]] && has "ERROR"; check "a crashed run (exit 101) is ERROR, exit 1" $? "rc=$rc"

# 11. The baseline: every filter of the shard must pass on the clean copy before any mutant runs.
write_manifest "A1${tab}kill.patch${tab}test(one)" "A2${tab}kill2.patch${tab}test(two)"
run 1/1
grep -q 'run saw=GUARD=on filter=(test(one)) | (test(two)) args=' "$tmp/stub.log"; check "baseline runs all of the shard's filters on the clean copy" $?
grep -- 'filter=(test(one))' "$tmp/stub.log" | grep -q -- '--no-tests=fail'; check "baseline passes --no-tests=fail" $?
STUB_BASE=run run 1/1
[[ $rc -eq 2 ]] && has "baseline failed"; check "a failing baseline exits 2" $? "rc=$rc"
! grep -q 'saw=GUARD=KILL' "$tmp/stub.log"; check "a failing baseline tries no mutant" $?
STUB_BASE=build run 1/1
[[ $rc -eq 2 ]] && has "baseline failed"; check "a baseline that does not build exits 2" $? "rc=$rc"
! grep -q 'saw=GUARD=KILL' "$tmp/stub.log"; check "a baseline that does not build tries no mutant" $?

# 12. An empty manifest exits 2; a shard with nothing while others have mutants exits 0.
write_manifest
run 1/4
[[ $rc -eq 2 ]]; check "a manifest with no mutants exits 2" $? "rc=$rc"
write_manifest "E1${tab}kill.patch${tab}f1" "E2${tab}kill2.patch${tab}f2"
run 3/4
[[ $rc -eq 0 ]]; check "an empty shard of a non-empty manifest exits 0" $? "rc=$rc"
run 1/4
[[ $rc -eq 0 ]] && has "E1"; check "the shard that has a mutant still runs it" $? "rc=$rc"

# 13. Manifest fields split on single tabs; empty fields are rejected.
write_manifest "T1${tab}${tab}kill.patch${tab}f1"
run 1/1
[[ $rc -eq 2 ]]; check "an empty middle field exits 2" $? "rc=$rc"
write_manifest "T2${tab}kill.patch${tab}"
run 1/1
[[ $rc -eq 2 ]]; check "an empty filter exits 2" $? "rc=$rc"
write_manifest "T3${tab}kill.patch${tab}f1${tab}extra"
run 1/1
[[ $rc -eq 2 ]]; check "an extra field exits 2" $? "rc=$rc"

# 14. A file left behind after the revert is caught.
write_manifest "U1${tab}stray.patch${tab}f1"
run 1/1
[[ $rc -eq 2 ]] && has "did not revert cleanly"; check "an untracked file after the revert exits 2" $? "rc=$rc"

echo "$pass passed, $fail failed"
[[ $fail -eq 0 ]]
