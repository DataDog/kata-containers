#!/usr/bin/env python3
"""Package KPMI's legacy ZIP interface from the same GitLab outputs as OCI."""

import argparse
import hashlib
from pathlib import Path
import shutil
import zipfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--input", type=Path, default=Path("."))
    parser.add_argument("--output", type=Path, default=Path("release-bundles"))
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    for arch in ("amd64", "arm64"):
        files = {
            "vmlinux": f"tools/packaging/kernel/vmlinux-{arch}",
            "kernel-config": f"tools/packaging/kernel/kernel-config-{arch}",
            "kata-containers-image-ubuntu.img": f"kata-rootfs-{arch}.img",
            "sbom.cdx.gz": f"kata-rootfs-{arch}.sbom.cdx.gz",
            "containerd-shim-kata-v2": f"containerd-shim-kata-v2-{arch}",
            "containerd-shim-kata-v2-rs": f"containerd-shim-kata-v2-rust-{arch}",
        }
        for source in files.values():
            if not (args.input / source).is_file() or not (args.input / source).stat().st_size:
                raise SystemExit(f"Missing or empty release artifact: {source}")
        archive = args.output / f"kata-artifacts-{arch}.zip"
        with zipfile.ZipFile(archive, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=6) as bundle:
            for name, source in files.items():
                # Fixed metadata makes retries produce identical ZIP digests.
                info = zipfile.ZipInfo(name, date_time=(1980, 1, 1, 0, 0, 0))
                info.compress_type = zipfile.ZIP_DEFLATED
                info.create_system = 3
                mode = 0o100755 if name.startswith("containerd-shim-") else 0o100644
                info.external_attr = mode << 16
                with (args.input / source).open("rb") as src, bundle.open(info, "w", force_zip64=True) as dst:
                    shutil.copyfileobj(src, dst)
        checksum = hashlib.sha256()
        with archive.open("rb") as stream:
            for chunk in iter(lambda: stream.read(1024 * 1024), b""):
                checksum.update(chunk)
        digest = checksum.hexdigest()
        (args.output / f"kata-checksum-{arch}.sha256").write_text(f"{digest}  {archive.name}\n")
        print(f"{archive}: sha256:{digest}", flush=True)


if __name__ == "__main__":
    main()
