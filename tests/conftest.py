"""Session setup for the Python suites.

Network-namespace policy (post-review hardening, finding 2): the governor REVIEWs evidence whose
extraction sandbox could not create a network namespace, unless GOVERNOR_ALLOW_NO_NETNS=1. Some CI
hosts (containers, kernels that restrict unprivileged user namespaces) cannot create one, which
would turn every end-to-end ALLOW into REVIEW for reasons unrelated to the test. On such a host,
and only there, this sets GOVERNOR_ALLOW_NO_NETNS=1 for the governors the tests start and says so
in the pytest header. Tests of the policy itself pass allow_no_netns explicitly.
"""
from __future__ import annotations

import os
import subprocess

from extractor import jail


def netns_available() -> bool:
    r, w = os.pipe()
    try:
        subprocess.run(jail.argv({"isolate_network": True, "netns_fd": w}, ["/bin/true"]), pass_fds=(w,),
                       env={"PATH": "/usr/bin:/bin"}, capture_output=True, timeout=20)
    except (OSError, subprocess.SubprocessError):
        return False
    finally:
        os.close(w)
    try:
        return os.read(r, 1) == b"1"
    finally:
        os.close(r)


NETNS = netns_available()
if not NETNS and "GOVERNOR_ALLOW_NO_NETNS" not in os.environ:
    os.environ["GOVERNOR_ALLOW_NO_NETNS"] = "1"


def pytest_report_header(config):
    if NETNS:
        return "extraction sandbox: network namespace available (fail-closed netns policy active)"
    return ("extraction sandbox: NO network namespace on this host; GOVERNOR_ALLOW_NO_NETNS="
            f"{os.environ.get('GOVERNOR_ALLOW_NO_NETNS')} for test governors")
