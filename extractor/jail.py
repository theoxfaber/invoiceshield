"""Exec wrapper that confines one hostile-input tool, without `preexec_fn`.

    python -I extractor/jail.py '<json config>' -- /abs/path/to/tool arg...

Why a wrapper (post-review hardening, finding 3a): `subprocess.Popen(preexec_fn=...)` runs Python
code between fork() and exec() in the child. In a threaded parent (the extraction service is a
ThreadingHTTPServer, the public sandbox app too) a lock held by another thread at fork time can
deadlock that child forever. Here the parent only does a plain fork+exec of this script
(`start_new_session=True` gives the new process group); this fresh, single-threaded interpreter
then applies the limits and exec()s the tool, so nothing runs in a half-forked child.

Config keys (all optional):
  cpu_secs, mem_bytes, fsize_bytes, nofile  -> RLIMIT_CPU / AS / FSIZE / NOFILE (cores always 0)
  isolate_network (bool)                    -> unshare(CLONE_NEWUSER | CLONE_NEWNET)
  netns_fd (int)                            -> write b"1"/b"0" (namespace obtained?) to this fd
The tool inherits only the environment the parent passed to this wrapper (the parent always
passes an explicit, scrubbed env). A failed unshare is reported loudly on stderr and through
netns_fd; it is never silent. Runs with `python -I`, so it imports only the standard library.
"""
from __future__ import annotations

import json
import os
import resource
import sys


JAIL_PATH = os.path.abspath(__file__)


def argv(cfg: dict, cmd: list[str]) -> list[str]:
    """Command line that runs `cmd` (absolute path first) inside this wrapper."""
    return [sys.executable, "-I", JAIL_PATH, json.dumps(cfg), "--", *cmd]


def _netns() -> tuple[bool, str]:
    try:
        os.unshare(os.CLONE_NEWUSER | os.CLONE_NEWNET)
        return True, ""
    except (OSError, AttributeError) as e:
        return False, f"{type(e).__name__}: {e}"


def main(argv: list[str]) -> int:
    if len(argv) < 4 or argv[2] != "--":
        sys.stderr.write("usage: jail.py '<json>' -- /abs/tool [args...]\n")
        return 126
    cfg = json.loads(argv[1])
    cmd = argv[3:]
    ok, why = False, "not requested"
    if cfg.get("isolate_network"):
        ok, why = _netns()
        if not ok:
            # Loud on purpose (finding 2): the governor also sees this through the signed
            # sandbox.network_namespace=false and routes the document to REVIEW by default.
            sys.stderr.write(f"jail: WARNING: no network namespace ({why}); the tool runs WITHOUT network isolation\n")
    fd = cfg.get("netns_fd")
    if fd is not None:
        os.write(int(fd), b"1" if ok else b"0")
        os.close(int(fd))
    for key, lim in (("cpu_secs", resource.RLIMIT_CPU), ("mem_bytes", resource.RLIMIT_AS),
                     ("fsize_bytes", resource.RLIMIT_FSIZE), ("nofile", resource.RLIMIT_NOFILE)):
        v = cfg.get(key)
        if v is not None:
            hard = int(v) + 1 if key == "cpu_secs" else int(v)
            resource.setrlimit(lim, (int(v), hard))
    resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
    try:
        os.execv(cmd[0], cmd)
    except OSError as e:
        sys.stderr.write(f"jail: exec {cmd[0]!r} failed: {e}\n")
        return 127
    return 0  # unreachable


if __name__ == "__main__":
    sys.exit(main(sys.argv))
