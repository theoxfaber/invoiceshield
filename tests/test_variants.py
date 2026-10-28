from dataset.gen.make_variants import build, manifest_hash


def test_variants_build_and_hash_stable(tmp_path):
    a = build(tmp_path / "a")
    assert len(a) == 72 and {b["variant"] for b in a} == {"t2", "scan", "photo"}
    assert manifest_hash(a) == manifest_hash(build(tmp_path / "b"))
