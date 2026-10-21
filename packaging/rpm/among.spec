Name:           among
Version:        0.1.2
Release:        1%{?dist}
Summary:        GNOME terminal multiplexer with project workspaces

License:        Apache-2.0
URL:            https://github.com/Among-Terminal/among
Source0:        %{name}-%{version}.tar.gz

# Each floor is the API level the Rust binding is compiled for (the highest
# vX_Y feature of gtk4, libadwaita and vte4 in Cargo.toml); the linked
# executable carries no version of its own for these libraries. check-rpm-spec
# refuses a floor below that level. Runtime requirements are the soname
# dependencies rpmbuild generates from the linked executables. Dynamically
# discovered WAV plugins need explicit requirements as well.
BuildRequires:  pkgconfig(gtk4) >= 4.12
BuildRequires:  pkgconfig(libadwaita-1) >= 1.5
BuildRequires:  pkgconfig(vte-2.91-gtk4) >= 0.76
BuildRequires:  cargo >= 1.98
BuildRequires:  rust
BuildRequires:  desktop-file-utils
BuildRequires:  libappstream-glib
BuildRequires:  librsvg2-tools
BuildRequires:  gzip
BuildRequires:  pkgconf-pkg-config
Requires:       librsvg2
Requires:       adwaita-icon-theme
Requires:       hicolor-icon-theme
Requires:       gsettings-desktop-schemas
# Fedora's gtk4 package includes the GStreamer media implementation (built into
# libgtk on current Fedora); WAV parsing and pulsesink are in plugins-good.
Requires:       gtk4 >= 4.12
Requires:       gstreamer1-plugins-good
# The media paintable uses GLES even for audio-only input; this dlopen is not a
# DT_NEEDED dependency and minimal Fedora installs otherwise abort in libepoxy.
Requires:       libglvnd-gles

%description
Organize live terminals in resizable panes and persistent project workspaces.
Optional external-process plugins provide clock, resource and directory status.
Plugin installation and enablement are explicit user actions.

%prep
%autosetup -n %{name}-%{version}

%build
# The product and build tooling use three independent Cargo packages.
# Build them in-tree and install into a staging root so the build does not
# require an installed Rust toolchain at run time.
export CARGO_HOME=%{_builddir}/cargo-home
export CARGO_TARGET_DIR=%{_builddir}/cargo-target
mkdir -p "$CARGO_HOME" "$CARGO_TARGET_DIR"
cargo build --release --locked --manifest-path Cargo.toml
cargo build --release --locked --manifest-path examples/plugins/Cargo.toml
cargo build --release --locked --manifest-path tools/among-tools/Cargo.toml --bin among-tools

%install
export CARGO_TARGET_DIR=%{_builddir}/cargo-target
rm -rf %{buildroot}
mkdir -p %{buildroot}%{_bindir}
mkdir -p %{buildroot}%{_datadir}/applications
mkdir -p %{buildroot}%{_datadir}/icons/hicolor/scalable/apps
mkdir -p %{buildroot}%{_datadir}/metainfo
mkdir -p %{buildroot}%{_datadir}/among/plugins

install -m 0755 "$CARGO_TARGET_DIR/release/among" %{buildroot}%{_bindir}/among

# Only the three shipped example plugins are packaged. They stay disabled by
# default, exactly as in the Snap and Debian packages.
for plugin in clock system-status pane-pwd; do
    install -m 0755 "$CARGO_TARGET_DIR/release/$plugin" \
        %{buildroot}%{_datadir}/among/plugins/$plugin
done

install -m 0644 data/com.github.among-terminal.desktop \
    %{buildroot}%{_datadir}/applications/
# The themed icon is installed in hicolor; a relative Icon path cannot resolve.
sed -i 's|^Exec=among$|Exec=/usr/bin/among|' \
    %{buildroot}%{_datadir}/applications/com.github.among-terminal.desktop
install -m 0644 data/com.github.among-terminal.svg \
    %{buildroot}%{_datadir}/icons/hicolor/scalable/apps/
install -m 0644 data/com.github.among-terminal.metainfo.xml \
    %{buildroot}%{_datadir}/metainfo/
"$CARGO_TARGET_DIR/release/among-tools" package-metainfo \
    data/com.github.among-terminal.metainfo.xml '%{version}' \
    %{buildroot}%{_datadir}/metainfo/com.github.among-terminal.metainfo.xml

%check
export CARGO_HOME=%{_builddir}/cargo-home
export CARGO_TARGET_DIR=%{_builddir}/cargo-target
cargo test --locked --manifest-path Cargo.toml
cargo test --locked --manifest-path examples/plugins/Cargo.toml
desktop-file-validate \
    %{buildroot}%{_datadir}/applications/com.github.among-terminal.desktop
appstream-util validate-relax \
    %{buildroot}%{_datadir}/metainfo/com.github.among-terminal.metainfo.xml

%files
%license LICENSE
%license NOTICE
%license THIRD_PARTY
%{_bindir}/among
%{_datadir}/applications/com.github.among-terminal.desktop
%{_datadir}/icons/hicolor/scalable/apps/com.github.among-terminal.svg
%{_datadir}/metainfo/com.github.among-terminal.metainfo.xml
%dir %{_datadir}/among
%{_datadir}/among/plugins

%changelog
* Mon Oct 19 2026 Among Terminal <hello+amongterminal@highload.zone> - 0.1.2~rc.1-1
- Use the approved square artwork and correct AppImageLauncher Wayland matching.
- Add public application screenshot metadata without claiming local-DEB catalogue support.

* Mon Oct 19 2026 Among Terminal <hello+amongterminal@highload.zone> - 0.1.1-1
- Remember resized sidebar width and remove bottom pop-up messages.
- Fix split directories, touchpad scrolling and workspace renaming.

* Sat Oct 10 2026 Among Terminal <hello+amongterminal@highload.zone> - 0.1.0-1
- Initial RPM packaging for Fedora 43 and 44 on amd64 and arm64.
