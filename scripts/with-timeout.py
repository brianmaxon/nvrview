"""Runs a command with an overall time limit and fails clearly on a hang.

    python3 -I scripts/with-timeout.py SECONDS LABEL COMMAND [ARGS...]

The command's output is passed through as it arrives. When the limit passes,
the command and everything it started (its process group) are stopped, the
last test the output showed as started is named, and the exit status is 124.
Otherwise the command's own exit status is returned.

macOS has no timeout(1), and `swift test` has no limit of its own.
"""

import os
import re
import signal
import subprocess
import sys
import threading

# Lines that announce a test starting, for XCTest ("Test Case '-[A b]'
# started.") and Swift Testing ("Test foo() started.").
STARTED = re.compile(r"(Test Case .* started|Test .* started)")


def main() -> int:
    if len(sys.argv) < 4:
        print(__doc__, file=sys.stderr)
        return 2
    seconds = float(sys.argv[1])
    label = sys.argv[2]
    command = sys.argv[3:]

    proc = subprocess.Popen(
        command,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        start_new_session=True,
        text=True,
        errors="replace",
    )
    last_started = [None]
    timed_out = threading.Event()

    def expire() -> None:
        timed_out.set()
        for sig in (signal.SIGTERM, signal.SIGKILL):
            try:
                os.killpg(proc.pid, sig)
            except ProcessLookupError:
                return
            try:
                proc.wait(timeout=5)
                return
            except subprocess.TimeoutExpired:
                continue

    timer = threading.Timer(seconds, expire)
    timer.start()
    assert proc.stdout is not None
    for line in proc.stdout:
        sys.stdout.write(line)
        sys.stdout.flush()
        if STARTED.search(line):
            last_started[0] = line.strip()
    status = proc.wait()
    timer.cancel()

    if timed_out.is_set():
        print(
            f"\nTIMEOUT: {label} did not finish within {seconds:.0f} s and was stopped.",
            file=sys.stderr,
        )
        if last_started[0]:
            print(f"last test started: {last_started[0]}", file=sys.stderr)
        return 124
    return status


if __name__ == "__main__":
    sys.exit(main())
