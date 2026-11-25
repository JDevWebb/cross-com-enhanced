#!/usr/bin/env python3
"""Checks a built ubiorbitapi_r2_loader.dll exports every Orbit method under the exact C++ name the
game imports (client/orbit/build.rs's EXPORTS), so a linker that mangles them fails the build
instead of the game ("entry point not found"):

    python3 scripts/check-exports.py path/to/ubiorbitapi_r2_loader.dll

Standard library only, so it runs on CI's Windows and Linux runners as they come.
"""
import re
import struct
import sys
from pathlib import Path


def exports(path):
    """The names a PE file exports."""
    data = Path(path).read_bytes()
    pe = struct.unpack_from("<I", data, 0x3C)[0]
    if data[pe : pe + 4] != b"PE\0\0":
        sys.exit(f"{path}: not a PE file")
    sections, optional_size = struct.unpack_from("<H12xH", data, pe + 6)
    optional = pe + 24
    pe32_plus = struct.unpack_from("<H", data, optional)[0] == 0x20B
    directories = optional + (112 if pe32_plus else 96)
    table = [struct.unpack_from("<8xIIII", data, optional + optional_size + 40 * i) for i in range(sections)]

    def offset(rva):
        for virtual_size, address, raw_size, raw in table:
            if address <= rva < address + max(virtual_size, raw_size):
                return rva - address + raw
        sys.exit(f"{path}: RVA {rva:#x} is in no section")

    def name_at(rva):
        start = offset(rva)
        return data[start : data.index(b"\0", start)].decode()

    export_rva = struct.unpack_from("<I", data, directories)[0]
    if not export_rva:
        return set()
    directory = offset(export_rva)
    count, names = struct.unpack_from("<I4xI", data, directory + 24)
    return {name_at(struct.unpack_from("<I", data, offset(names) + 4 * i)[0]) for i in range(count)}


def main():
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    build_rs = (Path(__file__).resolve().parent.parent / "client/orbit/build.rs").read_text()
    expected = set(re.findall(r'"(\?[^"]+)"', build_rs))
    if len(expected) < 15:
        sys.exit(f"check-exports: only {len(expected)} names found in client/orbit/build.rs")
    have = exports(sys.argv[1])
    missing = sorted(expected - have)
    if missing:
        print(f"check-exports: {sys.argv[1]} lacks {len(missing)} of the game's {len(expected)} imports:")
        for name in missing:
            print(f"  {name}")
        print(f"  (it exports: {', '.join(sorted(have)) or 'nothing'})")
        sys.exit(1)
    print(f"check-exports: all {len(expected)} Orbit methods exported by their C++ names")


if __name__ == "__main__":
    main()
