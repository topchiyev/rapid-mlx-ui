"""Evidence-only fixture audit. Uses temporary synthetic caches, no model assets.
Run with the inspected rapid-mlx installation's Python; not application code.
"""
import json
import tempfile
from pathlib import Path
from huggingface_hub import scan_cache_dir
from huggingface_hub.utils import _shared_blobs

with tempfile.TemporaryDirectory(prefix="rapid-cache-audit-") as temporary:
    root = Path(temporary)
    blob_hash = "a" * 64
    for repo, commits in [("models--fixture--remove", ["1" * 40, "2" * 40]), ("models--fixture--retain", ["3" * 40]), ("datasets--fixture--remove", ["4" * 40]), ("spaces--fixture--remove", ["5" * 40])]:
        directory = root / repo
        blob = directory / "blobs" / blob_hash
        blob.parent.mkdir(parents=True)
        blob.write_bytes(b"synthetic shared payload")
        assert _shared_blobs.publish_blob_to_shared_store(blob_path=str(blob), xet_hash=blob_hash, cache_dir=root, expected_size=24)
        for commit in commits:
            snapshot = directory / "snapshots" / commit
            snapshot.mkdir(parents=True)
            (snapshot / "weights.safetensors").symlink_to(Path("../../blobs") / blob_hash)
    session = root / "sessions.sqlite3"
    session.write_bytes(b"synthetic user state marker")
    source = root / "custom-source"
    source.mkdir()
    (source / "weights").write_bytes(b"leave intact")
    cache = scan_cache_dir(root)
    target = next(repo for repo in cache.repos if repo.repo_id == "fixture/remove" and repo.repo_type == "model")
    commits = sorted(rev.commit_hash for rev in target.revisions)
    strategy = cache.delete_revisions(*commits)
    strategy.execute()
    assert not (root / "models--fixture--remove").exists()
    retained = root / "models--fixture--retain/snapshots" / ("3" * 40) / "weights.safetensors"
    assert retained.read_bytes() == b"synthetic shared payload"
    assert (root / "datasets--fixture--remove").is_dir()
    assert (root / "spaces--fixture--remove").is_dir()
    assert session.read_bytes() == b"synthetic user state marker"
    assert (source / "weights").read_bytes() == b"leave intact"
    print(json.dumps({"runtime": "1.0.2", "kind": "synthetic temporary cache audit", "full_repository_revisions_removed": len(commits), "retained_shared_blob_readable": True, "datasets_spaces_custom_source_and_session_preserved": True, "expected_freed_bytes": strategy.expected_freed_size}, indent=2))
