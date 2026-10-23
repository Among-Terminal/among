FROM ubuntu@sha256:534baea6a22c03a63003dbc8dbe78fe34bc0d7e595d9a9dc9834884ff530eb55
ENV container=docker
RUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
    systemd systemd-sysv snapd squashfs-tools ca-certificates \
    xvfb openbox dbus-x11 xdotool x11-utils imagemagick procps pulseaudio pulseaudio-utils patchelf \
    libgtk-4-1 libadwaita-1-0 libvte-2.91-gtk4-0 librsvg2-common \
    && useradd --create-home among-runtime \
    && mkdir -p /var/lib/among-checks \
    && chmod 1777 /var/lib/among-checks \
    && rm -rf /var/lib/apt/lists/*
STOPSIGNAL SIGRTMIN+3
CMD ["/sbin/init"]
