#!/bin/sh
# Runs as root in a fresh Ubuntu 24.04 container on the architecture of the Snap.
# It downloads the exact sources the Snap inventory names and checks them against
# that inventory and the signed archive. A version the archive no longer offers
# fails here rather than at release.
# The inventory is bound at /input/native, among-tools at /input/among-tools.
set -eu
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq --no-install-recommends dpkg-dev ca-certificates ubuntu-keyring gpgv
mkdir /work
tools=/input/among-tools
native=/input/native/inventory.json
"$tools" collect-snap-sources "$native" /work/native-sources
"$tools" check-snap-sources "$native" /work/native-sources
"$tools" collect-apt-provenance /work/apt-authentication
"$tools" check-apt-provenance /work/apt-authentication "$native" /work/native-sources
"$tools" check-native-source-unpack /work/native-sources /work/native-unpack-audit >/dev/null
