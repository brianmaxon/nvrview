"""Runs a command with an overall time limit and fails clearly on a hang.

    python3 -I scripts/with-timeout.py SECONDS LABEL COMMAND [ARGS...]

The command's output is passed through as it arrives. When the limit passes,
the command and everything it started are stopped, the last test the output
showed as started is named, and the exit status is 124. Otherwise the
command's own exit status is returned.

"Everything it started" is the command's process group plus every descendant
found by parent links (a helper that made its own session is still found), so
a hung test helper never outlives its check. The same cleanup runs when this
wrapper is told to stop (TERM, HUP, INT), and a small watcher process does it
when the wrapper itself is killed without warning (KILL).

macOS has no timeout(1), and `swift test` has no limit of its own.
"""

import os
import re
import signal
import subprocess
import sys
import threading
import time

# Lines that announce a test starting, for XCTest ("Test Case '-[A b]'
# started.") and Swift Testing ("Test foo() started.").
STARTED = re.compile(r"(Test Case .* started|Test .* started)")


def descendants(root: int) -> list[int]:
    """Every process below `root`, from one snapshot of the process table."""
    out = subprocess.run(["ps", "-axo", "pid=,ppid="], capture_output=True, text=True).stdout
    children: dict[int, list[int]] = {}
    for line in out.splitlines():
        parts = line.split()
        if len(parts) == 2:
            children.setdefault(int(parts[1]), []).append(int(parts[0]))
    found: list[int] = []
    todo = [root]
    while todo:
        for child in children.get(todo.pop(), []):
            if child not in found:
                found.append(child)
                todo.append(child)
    return found


def alive(pid: int) -> bool:
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    return True


def group_alive(pgid: int) -> bool:
    try:
        os.killpg(pgid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    return True


def kill_tree(pgid: int, reap=lambda: None) -> None:
    """Stops the group `pgid` and all its descendants: TERM, then KILL."""
    for sig in (signal.SIGTERM, signal.SIGKILL):
        # The snapshot comes first: killing the parent reparents its children.
        pids = descendants(pgid)
        try:
            os.killpg(pgid, sig)
        except (ProcessLookupError, PermissionError):
            pass
        for pid in pids:
            try:
                os.kill(pid, sig)
            except (ProcessLookupError, PermissionError):
                pass
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline and (group_alive(pgid) or any(alive(p) for p in pids)):
            reap()
            time.sleep(0.05)
        if not group_alive(pgid) and not any(alive(p) for p in pids):
            return


def watch(wrapper: int, pgid: int) -> int:
    """Watcher: when the wrapper is gone, stop what it started."""
    while alive(wrapper):
        time.sleep(0.5)
    kill_tree(pgid)
    return 0


def main() -> int:
    if len(sys.argv) == 4 and sys.argv[1] == "--watch":
        return watch(int(sys.argv[2]), int(sys.argv[3]))
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
    # The watcher outlives a KILLed wrapper; it exits when the wrapper does.
    watcher = subprocess.Popen(
        [sys.executable, "-I", os.path.abspath(__file__), "--watch", str(os.getpid()), str(proc.pid)],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        start_new_session=True,
    )
    last_started = [None]
    timed_out = threading.Event()

    def expire() -> None:
        timed_out.set()
        kill_tree(proc.pid)

    def stop(signum, _frame) -> None:
        kill_tree(proc.pid, proc.poll)
        watcher.kill()
        os._exit(128 + signum)

    for sig in (signal.SIGTERM, signal.SIGHUP, signal.SIGINT):
        signal.signal(sig, stop)

    timer = threading.Timer(seconds, expire)
    timer.daemon = True
    timer.start()
    assert proc.stdout is not None

    def pump() -> None:
        for line in proc.stdout:
            sys.stdout.write(line)
            sys.stdout.flush()
            if STARTED.search(line):
                last_started[0] = line.strip()

    reader = threading.Thread(target=pump, daemon=True)
    reader.start()
    status = proc.wait()
    timer.cancel()
    # The command is done; anything it left running goes too (it may hold the
    # output pipe open, which would keep the reader waiting).
    if group_alive(proc.pid):
        kill_tree(proc.pid)
    reader.join(timeout=10)
    watcher.kill()

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
