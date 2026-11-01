#!/bin/sh
# Runs inside a fresh distribution container. It installs only the runtime
# packages the DEB or RPM declares for audio, plus the private null-sink and
# display tools the capture needs, then captures real output through the system
# GStreamer route. The bound build lives in /input, evidence goes to /output.
set -eu
if command -v apt-get >/dev/null; then
    export DEBIAN_FRONTEND=noninteractive
    apt-get update
    apt-get install -y --no-install-recommends \
        libgtk-4-1 libadwaita-1-0 libvte-2.91-gtk4-0 librsvg2-common \
        libgtk-4-media-gstreamer gstreamer1.0-plugins-good \
        pulseaudio pulseaudio-utils xvfb xauth openbox dbus-x11 x11-utils procps util-linux
    dpkg-query -W libgtk-4-media-gstreamer gstreamer1.0-plugins-good pulseaudio >/output/packages.txt
else
    dnf install -y gtk4 libadwaita vte291-gtk4 librsvg2 gstreamer1-plugins-good libglvnd-gles \
        pulseaudio pulseaudio-utils xorg-x11-server-Xvfb xorg-x11-xauth openbox dbus-x11 \
        xdpyinfo xprop procps-ng util-linux shadow-utils
    rpm -q gtk4 gstreamer1-plugins-good pulseaudio >/output/packages.txt
fi
useradd --create-home among-audio
mkdir /work
cp /input/audio-probe /input/among-tools /work/
chown -R among-audio /work
cd /work
status=0
runuser -u among-audio -- /work/among-tools check-audio-native /work/audio-probe /work/run || status=$?
cp -r /work/run/. /output/ 2>/dev/null || true
exit "$status"
