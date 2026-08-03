#!/usr/bin/env bash
#
# Proves the emitted SQL still runs on the oldest SQLite this project supports.
#
# The declared floor lives in one place, FLOOR below, and must match the version
# the README and the D3 decision state. The check is differential: every
# snapshot under tests/snapshots is executed against both the floor build and a
# recent build, and only a failure that appears on the floor alone is reported.
# That cancels out failures caused by extensions this harness does not load
# (sqlitegis, sqlite-vec) and by snapshots that are fragments of a larger
# script, neither of which says anything about the version.
#
# Usage: scripts/check_sqlite_floor.sh
set -euo pipefail

FLOOR=3460000        # 3.46.0
RECENT=3500200       # 3.50.2
FLOOR_URL="https://sqlite.org/2024/sqlite-amalgamation-${FLOOR}.zip"
RECENT_URL="https://sqlite.org/2025/sqlite-amalgamation-${RECENT}.zip"

repository="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
work="${SQLITE_FLOOR_CACHE:-${repository}/target/sqlite-floor}"
mkdir -p "${work}"

build_runner() {
    local version="$1" url="$2" directory="${work}/$1"
    if [[ -x "${directory}/runsql" ]]; then return; fi
    mkdir -p "${directory}"
    if [[ ! -f "${directory}/sqlite3.c" ]]; then
        curl -fsSL "${url}" -o "${directory}/amalgamation.zip"
        unzip -joq "${directory}/amalgamation.zip" -d "${directory}"
    fi
    cc -O1 -o "${directory}/runsql" "${repository}/scripts/runsql.c" "${directory}/sqlite3.c" \
        -I"${directory}" \
        -DSQLITE_ENABLE_FTS5 -DSQLITE_ENABLE_RTREE -DSQLITE_ENABLE_JSON1 \
        -DSQLITE_ENABLE_MATH_FUNCTIONS \
        -lpthread -ldl -lm
}

build_runner "${FLOOR}" "${FLOOR_URL}"
build_runner "${RECENT}" "${RECENT_URL}"

python3 - "${repository}" "${work}/corpus.sql" <<'PYTHON'
import pathlib
import sys

repository, destination = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2])
scripts = []
for snapshot in sorted((repository / "tests" / "snapshots").glob("*.snap")):
    parts = snapshot.read_text().split("---\n", 2)
    body = parts[2] if len(parts) >= 3 else ""
    statements = [line.rstrip() for line in body.splitlines() if line.strip()]
    if not statements:
        continue
    terminated = [s if s.endswith(";") else s + ";" for s in statements]
    scripts.append(f"-- {snapshot.name}\n" + "\n".join(terminated))
destination.write_text("\x01".join(scripts))
print(f"{len(scripts)} snapshot scripts", file=sys.stderr)
PYTHON

"${work}/${FLOOR}/runsql" "${work}/corpus.sql" | sort > "${work}/floor.txt"
"${work}/${RECENT}/runsql" "${work}/corpus.sql" | sort > "${work}/recent.txt"

if regressions="$(comm -23 "${work}/floor.txt" "${work}/recent.txt")" && [[ -z "${regressions}" ]]; then
    echo "emitted SQL runs on SQLite ${FLOOR}: no failure that a newer SQLite does not also have"
    exit 0
fi

echo "emitted SQL requires a SQLite newer than the declared floor ${FLOOR}:" >&2
echo "${regressions}" >&2
exit 1
