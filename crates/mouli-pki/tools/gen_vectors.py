#!/usr/bin/env python3
"""Generate signature test vectors with the OpenSSL command line.

Output lines (hex fields): kind curve-or-bits hash salt pubkey message signature expected
  kind: ecdsa | pkcs1 | pss ; expected: 1 valid, 0 must be rejected.
"""
import os, subprocess, sys, tempfile

def run(*a, inp=None):
    return subprocess.run(a, input=inp, check=True, capture_output=True).stdout

def main(out):
    lines = []
    msgs = [b"", b"abc", bytes(range(200)), b"MOULI signature test " * 20]
    with tempfile.TemporaryDirectory() as d:
        key = os.path.join(d, "k.pem")
        for curve, name in (("prime256v1", "p256"), ("secp384r1", "p384")):
            for _ in range(2):
                run("openssl", "ecparam", "-name", curve, "-genkey", "-noout", "-out", key)
                spki = run("openssl", "ec", "-in", key, "-pubout", "-outform", "DER")
                size = 65 if name == "p256" else 97
                pub = spki[-size:]
                for h in ("sha256", "sha384", "sha512"):
                    for m in msgs:
                        sig = run("openssl", "dgst", f"-{h}", "-sign", key, inp=m)
                        bad = bytearray(sig); bad[-3] ^= 1
                        lines.append(("ecdsa", name, h, 0, pub, m, sig, 1))
                        lines.append(("ecdsa", name, h, 0, pub, m, bytes(bad), 0))
                        lines.append(("ecdsa", name, h, 0, pub, m + b"x", sig, 0))
        for bits in (2048, 3072):
            run("openssl", "genrsa", "-out", key, str(bits))
            pub = run("openssl", "rsa", "-in", key, "-RSAPublicKey_out", "-outform", "DER")
            for h in ("sha256", "sha384", "sha512"):
                hl = {"sha256": 32, "sha384": 48, "sha512": 64}[h]
                for m in msgs:
                    sig = run("openssl", "dgst", f"-{h}", "-sign", key, inp=m)
                    bad = bytearray(sig); bad[10] ^= 4
                    lines.append(("pkcs1", str(bits), h, 0, pub, m, sig, 1))
                    lines.append(("pkcs1", str(bits), h, 0, pub, m, bytes(bad), 0))
                    lines.append(("pkcs1", str(bits), h, 0, pub, m + b"x", sig, 0))
                    sig = run("openssl", "dgst", f"-{h}", "-sign", key,
                              "-sigopt", "rsa_padding_mode:pss", "-sigopt", f"rsa_pss_saltlen:{hl}", inp=m)
                    bad = bytearray(sig); bad[10] ^= 4
                    lines.append(("pss", str(bits), h, hl, pub, m, sig, 1))
                    lines.append(("pss", str(bits), h, hl, pub, m, bytes(bad), 0))
                    lines.append(("pss", str(bits), h, hl + 1, pub, m, sig, 0))
    with open(out, "w") as f:
        for k, c, h, s, pub, m, sig, ok in lines:
            f.write(f"{k} {c} {h} {s} {pub.hex()} {m.hex() or '-'} {sig.hex()} {ok}\n")
    print(len(lines), "vectors")

main(sys.argv[1])
