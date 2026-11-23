#!/bin/sh
# Generate the test certificate chains in tests/data/pki with the OpenSSL command line.
# Keys are throwaway and discarded. Run from the crate directory: tools/gen_pki.sh
set -eu
OUT="tests/data/pki"
mkdir -p "$OUT"
T="$(mktemp -d)"
trap 'rm -rf "$T"' EXIT
DAYS=36500

ca_ext() { printf 'basicConstraints=critical,CA:TRUE%s\nkeyUsage=critical,keyCertSign,cRLSign\n' "${1:+,pathlen:$1}"; }
leaf_ext() { printf 'basicConstraints=CA:FALSE\nkeyUsage=critical,digitalSignature\nextendedKeyUsage=serverAuth\nsubjectAltName=DNS:example.test,DNS:*.wild.test,IP:192.0.2.1\n'; }

newkey() { # kind file
    case "$1" in
    ec384) openssl ecparam -name secp384r1 -genkey -noout -out "$2" ;;
    ec256) openssl ecparam -name prime256v1 -genkey -noout -out "$2" ;;
    rsa) openssl genrsa -out "$2" 2048 2>/dev/null ;;
    esac
}

issue() { # name kind subject issuer-name(or self) ext-file [sigopts...]
    name=$1; kind=$2; subj=$3; issuer=$4; ext=$5; shift 5
    newkey "$kind" "$T/$name.key"
    openssl req -new -key "$T/$name.key" -subj "$subj" -out "$T/$name.csr"
    if [ "$issuer" = self ]; then
        openssl x509 -req -in "$T/$name.csr" -signkey "$T/$name.key" -days $DAYS -extfile "$ext" "$@" -out "$OUT/$name.pem" 2>/dev/null
    else
        openssl x509 -req -in "$T/$name.csr" -CA "$OUT/$issuer.pem" -CAkey "$T/$issuer.key" -set_serial 0x$(openssl rand -hex 8) -days $DAYS -extfile "$ext" "$@" -out "$OUT/$name.pem" 2>/dev/null
    fi
}

ca_ext > "$T/ca.ext"; ca_ext 0 > "$T/ca0.ext"; leaf_ext > "$T/leaf.ext"

# Chain A: EC P-384 root -> EC P-256 intermediate (pathlen 0) -> EC P-256 leaf
issue ec-root ec384 "/CN=AEON Test EC Root" self "$T/ca.ext" -sha384
issue ec-int ec256 "/CN=AEON Test EC Intermediate" ec-root "$T/ca0.ext" -sha384
issue ec-leaf ec256 "/CN=leaf.example.test" ec-int "$T/leaf.ext" -sha256
# Chain B: RSA root -> RSA intermediate (pathlen 0) -> RSA leaf (PKCS#1 v1.5)
issue rsa-root rsa "/CN=AEON Test RSA Root" self "$T/ca.ext" -sha256
issue rsa-int rsa "/CN=AEON Test RSA Intermediate" rsa-root "$T/ca0.ext" -sha256
issue rsa-leaf rsa "/CN=leaf.example.test" rsa-int "$T/leaf.ext" -sha256
# Chain C: RSA leaf signed with PSS
issue pss-leaf rsa "/CN=leaf.example.test" rsa-int "$T/leaf.ext" -sha256 -sigopt rsa_padding_mode:pss -sigopt rsa_pss_saltlen:32 -sigopt rsa_mgf1_md:sha256
# Path length violation: root -> int1 (pathlen 0) -> int2 (CA) -> leaf
issue deep-int2 ec256 "/CN=AEON Test Too Deep" ec-int "$T/ca.ext" -sha256
issue deep-leaf ec256 "/CN=leaf.example.test" deep-int2 "$T/leaf.ext" -sha256
# Impostor: same subject as the real EC root, different key, signs a leaf
issue fake-root ec384 "/CN=AEON Test EC Root" self "$T/ca.ext" -sha384
issue fake-leaf ec256 "/CN=leaf.example.test" fake-root "$T/leaf.ext" -sha256
# Leaf that is not a server certificate (clientAuth only)
printf 'basicConstraints=CA:FALSE\nextendedKeyUsage=clientAuth\nsubjectAltName=DNS:example.test\n' > "$T/client.ext"
issue client-leaf ec256 "/CN=client" ec-int "$T/client.ext" -sha256
# Leaf with an unknown critical extension
printf 'basicConstraints=CA:FALSE\nsubjectAltName=DNS:example.test\n1.2.3.4.5=critical,ASN1:UTF8String:x\n' > "$T/crit.ext"
issue crit-leaf ec256 "/CN=crit" ec-int "$T/crit.ext" -sha256
# Leaf with a CA basicConstraints but no SAN (name check must fail)
printf 'basicConstraints=CA:FALSE\n' > "$T/nosan.ext"
issue nosan-leaf ec256 "/CN=example.test" ec-int "$T/nosan.ext" -sha256
ls "$OUT"
