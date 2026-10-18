#!/usr/bin/env python3
"""Exercise the actual app's GPU/fallback/error paths. Requires a macOS GUI session."""
import argparse
import json
import pathlib
import shutil
import subprocess
import tempfile

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("binary", type=pathlib.Path)
parser.add_argument("--report-dir", type=pathlib.Path, help="retain numeric positive-test diagnostics")
args = parser.parse_args()
binary = str(args.binary.resolve(strict=True))
with tempfile.TemporaryDirectory(prefix="lrd-smoke-") as folder:
    root = pathlib.Path(folder)
    for mode in ([], ["--software"]):
        report = root / ("fallback.json" if mode else "gpu.json")
        subprocess.run([binary, *mode, "--smoke-test", "--report", str(report)], check=True, timeout=25)
        result = json.loads(report.read_text())
        assert result["decoded_frames"] == 300, result
        assert result["rendered_frames"] > 0, result
        assert result["decode_or_gpu_errors"] == 0 and result["error"] == "", result
        assert result["remote_connection_tested"] is False
        if not mode:
            assert result["hardware_decode"] and result["renderer"] == "Metal", result
        else:
            assert result["renderer"] == "Core Image CPU fallback", result
        if args.report_dir:
            args.report_dir.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(report, args.report_dir / report.name)
        print(json.dumps(result, ensure_ascii=False))
    for source in (root / "missing.mp4", root / "corrupt.mp4"):
        if source.name == "corrupt.mp4":
            source.write_bytes(b"not an mp4")
        report = root / "error.json"
        completed = subprocess.run([binary, "--video", str(source), "--smoke-test", "--report", str(report)], timeout=25)
        assert completed.returncode != 0, source
        result = json.loads(report.read_text())
        assert result["error"] and result["decoded_frames"] == 0, result
        print("PASS rejected", source.name)
    destination = root / "nonexistent-directory/report.json"
    completed = subprocess.run([binary, "--smoke-test", "--report", str(destination)], timeout=25)
    assert completed.returncode == 6, completed.returncode
    print("PASS report write failure")
