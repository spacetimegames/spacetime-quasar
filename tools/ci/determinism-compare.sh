#!/usr/bin/env bash
# Cross-target determinism comparison (ADR 0003, ADR 0008).
#
# usage: determinism-compare.sh <artifacts-dir> <reference-file>
#
# <artifacts-dir> is where actions/download-artifact put the artifacts
# `determinism-<platform>`, one directory each, with one file inside named
# `determinism-<platform>.txt`. Exactly the four expected files must exist.
# Each is compared byte for byte with the reference file and with every other
# one. Any mismatch prints the first diverging scenario and tick per target
# and exits 1.
#
# Guards against a vacuous pass (four empty files are equal to an empty
# reference): the reference and every output must be non-empty, end with a
# newline, and have at least one line for each of at least MIN_SCENARIOS
# scenarios of the reference (the harness has three). Each artifact directory
# must hold exactly its one expected file. When GITHUB_STEP_SUMMARY is set, a summary table is appended.
#
# Exit status: 0 all equal, 1 mismatch or a missing/extra file, 2 usage.

set -euo pipefail

if [ "$#" -ne 2 ]; then
  echo "usage: $0 <artifacts-dir> <reference-file>" >&2
  exit 2
fi
dir=$1
reference=$2
platforms=(linux-x86_64 windows-x86_64 macos-aarch64 wasm32)
failed=0
# The harness has three scenarios (docs/62-determinism-harness.adoc); raise
# this when scenarios are added so that a truncated reference cannot pass.
MIN_SCENARIOS=3

if [ ! -f "$reference" ]; then
  echo "error: reference file $reference does not exist" >&2
  exit 1
fi

# Checks that a file is a plausible harness output: non-empty, ends with a
# newline, and has at least one line for every scenario in $2 (the reference;
# for the reference itself, at least MIN_SCENARIOS scenarios). Prints a reason
# and returns 1 if not.
plausible() {
  local file=$1 want=$2 name=$3 scenario
  if [ ! -s "$file" ]; then
    echo "EMPTY: $name has no content"
    return 1
  fi
  if [ -n "$(tail -c 1 "$file")" ]; then
    echo "TRUNCATED: $name does not end with a newline"
    return 1
  fi
  local count
  count=$(cut -d' ' -f1 "$want" | sort -u | wc -l)
  if [ "$count" -lt "$MIN_SCENARIOS" ]; then
    echo "TOO FEW SCENARIOS: the reference has $count, at least $MIN_SCENARIOS are required"
    return 1
  fi
  while read -r scenario; do
    if ! grep -q "^$scenario " "$file"; then
      echo "MISSING SCENARIO: $name has no line for $scenario"
      return 1
    fi
  done < <(cut -d' ' -f1 "$want" | sort -u)
}

# Prints the first line where two files differ, as the scenario and tick it
# belongs to, plus how many lines differ. Prints nothing if they are equal.
# Lines are `<scenario> <tick> <hash>`, sorted, so the first differing line
# is the first diverging scenario and tick.
first_difference() {
  awk -v an="$3" -v bn="$4" '
    NR == FNR { a[FNR] = $0; na = FNR; next }
    { b[FNR] = $0; nb = FNR }
    END {
      n = na > nb ? na : nb
      bad = 0
      for (i = 1; i <= n; i++) {
        if (a[i] == b[i] && (i <= na) == (i <= nb)) continue
        bad++
        if (bad > 1) continue
        line = (i <= na) ? a[i] : b[i]
        split(line, f, " ")
        first = sprintf("first difference at line %d (scenario %s, tick %s)\n    %-22s %s\n    %-22s %s",
          i, f[1], f[2],
          an, (i <= na) ? a[i] : "(file ends here)",
          bn, (i <= nb) ? b[i] : "(file ends here)")
      }
      if (bad > 0) printf "%s\n    %d of %d lines differ\n", first, bad, n
    }' "$1" "$2"
}

# Every artifact directory must be an expected one, and every expected file
# must be there.
if ! plausible "$reference" "$reference" "the reference file"; then
  failed=1
fi
declare -A path
for p in "${platforms[@]}"; do
  path[$p]="$dir/determinism-$p/determinism-$p.txt"
  if [ ! -f "${path[$p]}" ]; then
    echo "MISSING: no harness output for $p (expected ${path[$p]})"
    failed=1
  elif ! plausible "${path[$p]}" "$reference" "$p"; then
    failed=1
  fi
  # Nothing but the expected file may be in the artifact directory.
  for stray in "$dir/determinism-$p"/* "$dir/determinism-$p"/.[!.]*; do
    [ -e "$stray" ] || continue
    if [ "$stray" != "${path[$p]}" ]; then
      echo "UNEXPECTED: $stray is not part of the $p artifact"
      failed=1
    fi
  done
done
for found in "$dir"/determinism-*; do
  [ -e "$found" ] || continue
  known=0
  for p in "${platforms[@]}"; do
    [ "$found" = "$dir/determinism-$p" ] && known=1
  done
  if [ "$known" = 0 ]; then
    echo "UNEXPECTED: $found is not one of the four expected artifacts"
    failed=1
  fi
done

declare -A against_ref
for p in "${platforms[@]}"; do
  [ -f "${path[$p]}" ] || { against_ref[$p]="missing"; continue; }
  if cmp -s "$reference" "${path[$p]}"; then
    against_ref[$p]="equal"
  else
    against_ref[$p]="DIFFERS"
    failed=1
    echo "MISMATCH: $p differs from the reference"
    first_difference "$reference" "${path[$p]}" reference "$p"
  fi
done

# Pairwise. When a target already differs from the reference the pairs that
# involve it are expected to differ too, but two targets that agree with each
# other and not with the reference are a sign of a stale reference file.
for ((i = 0; i < ${#platforms[@]}; i++)); do
  for ((j = i + 1; j < ${#platforms[@]}; j++)); do
    a=${platforms[$i]}
    b=${platforms[$j]}
    [ -f "${path[$a]}" ] && [ -f "${path[$b]}" ] || continue
    if ! cmp -s "${path[$a]}" "${path[$b]}"; then
      failed=1
      echo "MISMATCH: $a and $b differ"
      first_difference "${path[$a]}" "${path[$b]}" "$a" "$b"
    fi
  done
done

if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
  {
    echo "### Determinism"
    echo
    echo "| Target | Lines | Against the reference |"
    echo "| --- | --- | --- |"
    for p in "${platforms[@]}"; do
      lines=-
      [ -f "${path[$p]}" ] && lines=$(wc -l < "${path[$p]}")
      echo "| $p | $lines | ${against_ref[$p]} |"
    done
    echo
    if [ "$failed" = 0 ]; then
      echo "All four targets are byte-identical to each other and to \`tools/determinism/reference.txt\`."
    else
      echo "**Mismatch.** See the job log for the first diverging scenario and tick per target."
    fi
  } >> "$GITHUB_STEP_SUMMARY"
fi

if [ "$failed" != 0 ]; then
  cat <<'MSG'

The simulation is not bit-identical across targets. To find the cause, run the
harness with `--checkpoint-every 1` on the two targets, find the first tick
that differs, then run both with `--dump-tick <tick> --dump <file>` and diff
the dump files (docs/62-determinism-harness.adoc). If every target agrees with
the others but not with the reference and the change was meant to alter the
simulation, regenerate tools/determinism/reference.txt and say why in the PR.
MSG
  exit 1
fi
echo "OK: ${#platforms[@]} targets are byte-identical to each other and to $reference ($(wc -l < "$reference") lines)."
