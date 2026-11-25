#!/usr/bin/env bash
# Creates "bricolo dev", a self-signed code-signing identity in the
# login keychain, for signing development builds of bricolo.app
# (design Decision 5). macOS keys the Accessibility, Input Monitoring and
# Microphone grants on the app's signing identity, so with one stable
# identity the grants survive rebuilds. Ad-hoc signatures change with every
# build, and macOS asks again.
#
#   scripts/macos-dev-identity.sh          create it, or say it exists
#   scripts/macos-dev-identity.sh --hash   print its SHA-1, or nothing
#
# Running it again changes nothing. It adds one certificate and its private
# key to the login keychain and touches no other item. It then asks macOS to
# trust the certificate for code signing, which needs your password in a
# dialog. Signing works without that trust; `security find-identity -v`
# just leaves the identity out. To remove it: Keychain Access, login
# keychain, My Certificates, delete "bricolo dev".
set -euo pipefail

name="${BRICOLO_DEV_IDENTITY:-bricolo dev}"
keychain="$HOME/Library/Keychains/login.keychain-db"

# The SHA-1 of the identity (certificate and key) with this name.
identity_hash() {
    security find-identity -p codesigning "$keychain" 2>/dev/null |
        grep -F "\"$name\"" | awk '{print $2}' | head -1 || true
}

if [ "${1:-}" = --hash ]; then
    identity_hash
    exit 0
fi

existing="$(identity_hash)"
if [ -n "$existing" ]; then
    echo "exists: \"$name\" ($existing) in $keychain"
    exit 0
fi

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

cat >"$work/openssl.cnf" <<EOF
[req]
distinguished_name = dn
x509_extensions = ext
prompt = no
[dn]
CN = $name
[ext]
basicConstraints = critical, CA:false
keyUsage = critical, digitalSignature
extendedKeyUsage = critical, codeSigning
subjectKeyIdentifier = hash
EOF

# macOS's own LibreSSL writes a PKCS#12 file `security import` reads; the
# OpenSSL 3 default ciphers need -legacy for that.
ssl=/usr/bin/openssl
"$ssl" req -x509 -newkey rsa:2048 -nodes -days 3650 -config "$work/openssl.cnf" \
    -keyout "$work/key.pem" -out "$work/cert.pem" 2>/dev/null
password="$("$ssl" rand -hex 16)"
"$ssl" pkcs12 -export -inkey "$work/key.pem" -in "$work/cert.pem" -name "$name" \
    -out "$work/identity.p12" -passout "pass:$password"

echo "creating in $keychain:"
echo "  certificate \"$name\" (self-signed, code signing only, valid 10 years)"
echo "  its private key, usable by /usr/bin/codesign without a prompt"
security import "$work/identity.p12" -k "$keychain" -P "$password" -T /usr/bin/codesign >/dev/null

created="$(identity_hash)"
[ -n "$created" ] || {
    echo "error: the import finished but no identity \"$name\" is listed" >&2
    exit 1
}
echo "created: \"$name\" ($created)"

# Trust for code signing, in your user trust settings. macOS asks for your
# password in a dialog; with no one to answer it this fails after a while.
echo "asking macOS to trust it for code signing (a password dialog may open)"
if timeout 60 security add-trusted-cert -r trustRoot -p codeSign -k "$keychain" "$work/cert.pem" 2>"$work/trust.err"; then
    echo "trusted for code signing"
else
    echo "not trusted ($(tr -d '\n' <"$work/trust.err")). Signing still works;"
    echo "macOS keys permission grants on the certificate, not on trust."
fi
