"""Owned native-test parent; killed by the Rust test using its Child handle.

Bootstrap/signing seed travel through anonymous stdin only, never a file/argv.
The actual larm-daemon helper survives this parent's abrupt termination.
"""
import json
import subprocess
import sys

line = sys.stdin.buffer.readline(512 * 1024 + 2)
assert line.endswith(b"\n") and len(line) <= 512 * 1024 + 1
helper = subprocess.Popen(
    [sys.argv[1], "worker-guardian"],
    stdin=subprocess.PIPE,
    stdout=subprocess.DEVNULL,
    stderr=None,
    creationflags=subprocess.CREATE_NO_WINDOW,
    close_fds=True,
)
helper.stdin.write(line)
helper.stdin.flush()
print(json.dumps({"helper_pid": helper.pid}), flush=True)
sys.stdin.buffer.read()
helper.stdin.close()
assert helper.wait(timeout=30) == 0
