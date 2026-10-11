Name:           among
Version:        0.1.0
Release:        1%{?dist}
Summary:        GNOME terminal multiplexer with project workspaces

License:        Apache-2.0
URL:            https://github.com/Among-Terminal/among
Source0:        %{name}-%{version}.tar.gz

# BuildRequires mirrors the Debian package's declared minimum versions. The
# pkgconfig capabilities are the Fedora way to express the same requirement and
# let RPM resolve the linked shared objects automatically at install time.
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

%description
Organize live terminals in resizable panes and persistent project workspaces.
Optional external-process plugins provide clock, resource and directory status.
Plugin installation and enablement are explicit user actions.

%prep
%autosetup -n %{name}-%{version}

%build
# The application and the example plugins are three independent Cargo packages.
# Build them in-tree and install into a staging root so the build does not
# require an installed Rust toolchain at run time.
export CARGO_HOME=%{_builddir}/cargo-home
export CARGO_TARGET_DIR=%{_builddir}/cargo-target
mkdir -p "$CARGO_HOME" "$CARGO_TARGET_DIR"
cargo build --release --locked --manifest-path Cargo.toml
cargo build --release --locked --manifest-path examples/plugins/Cargo.toml

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
%doc NOTICE
%doc THIRD_PARTY
%{_bindir}/among
%{_datadir}/applications/com.github.among-terminal.desktop
%{_datadir}/icons/hicolor/scalable/apps/com.github.among-terminal.svg
%{_datadir}/metainfo/com.github.among-terminal.metainfo.xml
%{_datadir}/among/plugins

%changelog
* Sat Oct 10 2026 Among Terminal <hello+amongterminal@highload.zone> - 0.1.0-1
- Initial RPM packaging for Fedora 43 and 44 on amd64 and arm64.
