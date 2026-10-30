"""docs/openapi.yaml must describe every route the governor server registers."""
import pathlib
import re

ROOT = pathlib.Path(__file__).resolve().parents[1]


def server_routes() -> set[str]:
    src = "".join((ROOT / "governor" / "src" / f).read_text() for f in ("bin/server.rs", "observability.rs"))
    return {re.sub(r":(\w+)", r"{\1}", p) for p in re.findall(r'\.route\(\s*"([^"]+)"', src)}


def documented_paths() -> set[str]:
    text = (ROOT / "docs" / "openapi.yaml").read_text()
    body = text.split("\npaths:\n", 1)[1].split("\ncomponents:\n", 1)[0]
    return set(re.findall(r"^  (/[^:\s]*):\s*$", body, flags=re.M))


def test_every_route_is_documented():
    routes = server_routes()
    assert "/v1/actions" in routes and "/metrics" in routes, routes
    missing = routes - documented_paths()
    assert not missing, f"routes missing from docs/openapi.yaml: {sorted(missing)}"


def test_decision_routes_are_versioned():
    unversioned = {r for r in server_routes() if r not in {"/health", "/healthz", "/readyz", "/metrics"} and not r.startswith("/v1/")}
    assert not unversioned, unversioned
