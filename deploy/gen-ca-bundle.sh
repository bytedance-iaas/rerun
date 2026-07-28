#!/bin/sh
# Exports the macOS keychain certs (incl. the corporate proxy CA) to
# deploy/ca-bundle.pem so cargo/rustup inside docker can pass the
# TLS-intercepting proxy. Run once before the first `docker compose build`.
# /tmp is wiped on reboot, hence a repo-local (gitignored) copy.
set -eu
cd "$(dirname "$0")"

: > ca-bundle.pem
for kc in \
    /System/Library/Keychains/SystemRootCertificates.keychain \
    /Library/Keychains/System.keychain \
    "$HOME/Library/Keychains/login.keychain-db"; do
    security find-certificate -a -p "$kc" >> ca-bundle.pem 2>/dev/null || true
done

echo "wrote $(grep -c 'BEGIN CERTIFICATE' ca-bundle.pem) certs to deploy/ca-bundle.pem"
