#!/usr/bin/env python3
"""Validate a corrective book against the running GUI.

For every entry in the book file (default corrective-book.txt), the
position is pushed onto the GUI via `bookview --replay <key>`; the engine
(yellow, to move in every corrective position) must answer instantly with
the booked column, sourced from the book.

Prerequisites: the GUI must be running from the repository root and must
have been started AFTER the book file was last extended (books are loaded
once at startup). The script drives the shared board - don't run it while
a game you care about is in progress.

Usage: python3 scripts/validate_book.py [book-file]
"""
import re
import subprocess
import sys

book = sys.argv[1] if len(sys.argv) > 1 else "corrective-book.txt"
entries = []
for line in open(book):
    parts = line.split()
    if len(parts) == 3:
        entries.append((parts[0], int(parts[1])))
print(f"validating {len(entries)} entries from {book}")

failures = []
for i, (key, col) in enumerate(entries, 1):
    out = subprocess.run(
        ["./target/release/bookview", "--replay", key],
        capture_output=True, text=True
    ).stdout
    m = re.search(r"engine answered col (\d+)( \(from book\))?", out)
    if not m:
        failures.append((key, col, "engine did not answer (GUI running? game over?)"))
    elif int(m.group(1)) != col:
        failures.append((key, col, f"engine played col {m.group(1)} instead"))
    elif not m.group(2):
        failures.append((key, col, "right column, but from search, not from the book"))
    if i % 10 == 0:
        print(f"  {i}/{len(entries)} checked, {len(failures)} failures")

if failures:
    print(f"\nFAIL: {len(failures)} of {len(entries)} entries:")
    for key, col, why in failures:
        print(f"  {key} (book col {col}): {why}")
    sys.exit(1)
print(f"\nOK: all {len(entries)} book entries answered by the engine, from the book")
