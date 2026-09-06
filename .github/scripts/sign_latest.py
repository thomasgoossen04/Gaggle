#!/usr/bin/env python3
"""Sign the release descriptor with the Ed25519 release key.

Usage: sign_latest.py <descriptor-path>

Reads a 64-hex-char raw Ed25519 seed from ``$GAGGLE_RELEASE_SIGNING_KEY`` (set
it as a repository secret), signs ``DOMAIN || <descriptor bytes>``, and writes
``<descriptor-path>.sig`` — 128 lowercase-hex chars, a detached signature.

Both launchers verify this before acting on the descriptor: the matching public
key is compiled in at
``crates/launcher/src/signing.rs::RELEASE_PUBLIC_KEY_HEX`` (and the identical
copy in ``crates/accelerator-launcher``). The private seed here and that public
key must be a pair — this script prints the public key it derived so a
mismatch is obvious in the CI log.

Rotating the key: generate a new seed, set the secret, replace
``RELEASE_PUBLIC_KEY_HEX`` in both crates (and the pinned test vectors), and cut
a release. Clients only trust the new key once they have updated to a build
carrying it, so keep the old key usable until that has propagated.
"""

import os
import pathlib
import sys

DOMAIN = b"gaggle-release-descriptor-v1\n"


def main() -> int:
    if len(sys.argv) != 2:
        print(__doc__, file=sys.stderr)
        return 2

    try:
        from cryptography.hazmat.primitives import serialization
        from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
    except ImportError:
        print("error: the `cryptography` package is required (pip install cryptography)", file=sys.stderr)
        return 1

    key_hex = os.environ.get("GAGGLE_RELEASE_SIGNING_KEY", "").strip()
    if len(key_hex) != 64:
        print(
            "error: $GAGGLE_RELEASE_SIGNING_KEY must be exactly 64 hex chars "
            "(a raw Ed25519 seed). Set it as a repository secret; the release "
            "descriptor is not published unsigned.",
            file=sys.stderr,
        )
        return 1
    try:
        seed = bytes.fromhex(key_hex)
    except ValueError:
        print("error: $GAGGLE_RELEASE_SIGNING_KEY is not valid hex", file=sys.stderr)
        return 1

    key = Ed25519PrivateKey.from_private_bytes(seed)
    pub_hex = key.public_key().public_bytes(
        serialization.Encoding.Raw, serialization.PublicFormat.Raw
    ).hex()

    path = pathlib.Path(sys.argv[1])
    body = path.read_bytes()
    sig = key.sign(DOMAIN + body)

    out = path.with_name(path.name + ".sig")
    out.write_text(sig.hex())
    print(f"signed {path} -> {out} ({len(sig.hex())} hex chars)")
    print(f"release public key: {pub_hex}")
    print("  (must equal RELEASE_PUBLIC_KEY_HEX in crates/*/src/signing.rs)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
