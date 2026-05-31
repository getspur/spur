#!/usr/bin/env python3
"""Append DuckDB extension metadata to a native shared library.

This is a small, local copy of the footer layout used by DuckDB's
extension-ci-tools append_extension_metadata.py script:
https://github.com/duckdb/extension-ci-tools/blob/main/scripts/append_extension_metadata.py
"""

from __future__ import annotations

import argparse
import shutil


FIELD_SIZE = 32
SIGNATURE_SIZE = 256


def start_signature() -> bytes:
    # Prefix used by DuckDB's tooling so Wasm binaries remain valid after the
    # DuckDB metadata footer is appended. Native extensions use the same footer.
    encoded = b""
    encoded += int(0).to_bytes(1, byteorder="big")
    encoded += int(147).to_bytes(1, byteorder="big")
    encoded += int(4).to_bytes(1, byteorder="big")
    encoded += int(16).to_bytes(1, byteorder="big")
    encoded += b"duckdb_signature"
    encoded += int(128).to_bytes(1, byteorder="big")
    encoded += int(4).to_bytes(1, byteorder="big")
    return encoded


def padded_ascii(value: str) -> bytes:
    encoded = value.encode("ascii")
    if len(encoded) > FIELD_SIZE:
        raise ValueError(f"metadata field is too long for DuckDB footer: {value!r}")
    return encoded + (b"\x00" * (FIELD_SIZE - len(encoded)))


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Append metadata to a loadable DuckDB extension"
    )
    parser.add_argument("-l", "--library-file", required=True)
    parser.add_argument("-n", "--extension-name", required=True)
    parser.add_argument("-o", "--out-file", default="")
    parser.add_argument("-p", "--duckdb-platform", required=True)
    parser.add_argument("-dv", "--duckdb-version", required=True)
    parser.add_argument("-ev", "--extension-version", required=True)
    parser.add_argument("--abi-type", default="C_STRUCT")
    args = parser.parse_args()

    out_file = args.out_file or f"{args.extension_name}.duckdb_extension"
    tmp_file = f"{out_file}.tmp"
    shutil.copyfile(args.library_file, tmp_file)

    print("Creating extension binary:")
    print(f" - Input file: {args.library_file}")
    print(f" - Output file: {out_file}")
    print(" - Metadata:")
    print(" - FIELD8 (unused) = EMPTY")
    print(" - FIELD7 (unused) = EMPTY")
    print(" - FIELD6 (unused) = EMPTY")
    print(f" - FIELD5 (abi_type) = {args.abi_type}")
    print(f" - FIELD4 (extension_version) = {args.extension_version}")
    print(f" - FIELD3 (duckdb_version) = {args.duckdb_version}")
    print(f" - FIELD2 (duckdb_platform) = {args.duckdb_platform}")
    print(" - FIELD1 (header signature) = 4")

    with open(tmp_file, "ab") as file:
        file.write(start_signature())
        file.write(padded_ascii(""))
        file.write(padded_ascii(""))
        file.write(padded_ascii(""))
        file.write(padded_ascii(args.abi_type))
        file.write(padded_ascii(args.extension_version))
        file.write(padded_ascii(args.duckdb_version))
        file.write(padded_ascii(args.duckdb_platform))
        file.write(padded_ascii("4"))
        file.write(b"\x00" * SIGNATURE_SIZE)

    shutil.move(tmp_file, out_file)


if __name__ == "__main__":
    main()

