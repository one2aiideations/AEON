#!/usr/bin/env python3
"""Generate cross-check vectors for mouli-crypto from an independent implementation.

Uses the `cryptography` package (OpenSSL). Output: tests/vectors.txt, one vector per line:
  x25519 <scalar> <u> <out>
  aead   <key> <nonce> <aad> <plaintext> <sealed>
  hmac   <key> <msg> <mac>
  hkdf   <salt> <ikm> <info> <len> <okm>
Fixed seed, so the file is reproducible.
"""
import hmac, hashlib, random, sys
from cryptography.hazmat.primitives.asymmetric.x25519 import X25519PrivateKey, X25519PublicKey
from cryptography.hazmat.primitives.ciphers.aead import ChaCha20Poly1305
from cryptography.hazmat.primitives.kdf.hkdf import HKDF
from cryptography.hazmat.primitives import hashes

rng = random.Random(0xAE0C)
rb = lambda n: bytes(rng.randrange(256) for _ in range(n))
out = []
for _ in range(40):
    k, u = rb(32), rb(32)
    # clamping and the top-bit mask are part of X25519, applied by both sides
    try:
        shared = X25519PrivateKey.from_private_bytes(k).exchange(X25519PublicKey.from_public_bytes(u))
    except ValueError:
        continue  # all-zero output (low-order point): the library refuses it
    out.append(f"x25519 {k.hex()} {u.hex()} {shared.hex()}")
for n in list(range(0, 70)) + [127, 128, 129, 255, 256, 257, 1000]:
    key, nonce, aad, pt = rb(32), rb(12), rb(rng.randrange(0, 40)), rb(n)
    out.append(f"aead {key.hex()} {nonce.hex()} {aad.hex() or '-'} {pt.hex() or '-'} {ChaCha20Poly1305(key).encrypt(nonce, pt, aad or None).hex()}")
for klen, mlen in [(0, 0), (1, 5), (20, 8), (64, 64), (65, 10), (131, 54), (32, 200)]:
    key, msg = rb(klen), rb(mlen)
    out.append(f"hmac {key.hex() or '-'} {msg.hex() or '-'} {hmac.new(key, msg, hashlib.sha256).hexdigest()}")
for slen, ilen, inflen, ln in [(0, 22, 0, 42), (13, 22, 10, 42), (32, 32, 12, 100), (5, 64, 3, 255), (16, 16, 0, 1)]:
    salt, ikm, info = rb(slen), rb(ilen), rb(inflen)
    okm = HKDF(algorithm=hashes.SHA256(), length=ln, salt=salt or None, info=info).derive(ikm)
    out.append(f"hkdf {salt.hex() or '-'} {ikm.hex()} {info.hex() or '-'} {ln} {okm.hex()}")
open(sys.argv[1] if len(sys.argv) > 1 else "tests/vectors.txt", "w").write("\n".join(out) + "\n")
print(len(out), "vectors")
