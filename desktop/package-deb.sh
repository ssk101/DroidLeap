#!/bin/sh
# Builds target/debian/droidleap_<version>_<arch>.deb from this crate.
set -eu
umask 022
cd "$(dirname "$0")"

NAME=droidleap
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
ARCH=$(dpkg --print-architecture)
OUT=target/debian
STAGE=$OUT/$NAME

cargo build --release

rm -rf "$STAGE"
mkdir -p "$STAGE/DEBIAN" "$STAGE/usr/bin" "$STAGE/usr/share/applications" \
    "$STAGE/usr/share/icons/hicolor/256x256/apps" "$STAGE/usr/share/doc/$NAME"

install -s -m 755 "target/release/$NAME" "$STAGE/usr/bin/$NAME"
install -m 644 ../icon.png "$STAGE/usr/share/icons/hicolor/256x256/apps/$NAME.png"

cat > "$STAGE/usr/share/applications/$NAME.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=DroidLeap
Comment=Move the mouse off a screen edge to control an Android device over USB
Exec=$NAME
Icon=$NAME
Terminal=false
Categories=Utility;
EOF

{
    echo "DroidLeap"
    echo
    echo "HID report descriptors are taken from scrcpy:"
    echo "Copyright (C) 2018 Genymobile, Copyright (C) 2018-2025 Romain Vimont,"
    echo "licensed under the Apache License 2.0 (/usr/share/common-licenses/Apache-2.0)."
    echo
    echo "Everything else:"
    cat ../LICENSE
} > "$STAGE/usr/share/doc/$NAME/copyright"

# dpkg-shlibdeps needs a debian/control next to the files it inspects.
mkdir -p "$OUT/debian"
printf 'Source: %s\n' "$NAME" > "$OUT/debian/control"
SHLIBS=$(cd "$OUT" && dpkg-shlibdeps -O "$NAME/usr/bin/$NAME" | sed 's/^shlibs:Depends=//')

cat > "$STAGE/DEBIAN/control" <<EOF
Package: $NAME
Version: $VERSION
Architecture: $ARCH
Maintainer: Stian Skjondal <ssk101@users.noreply.github.com>
Depends: $SHLIBS, xdg-desktop-portal, android-udev-rules
Recommends: xdg-desktop-portal-gnome | xdg-desktop-portal-kde, gnome-shell-extension-appindicator | plasma-workspace
Conflicts: tablet-edge
Replaces: tablet-edge
Section: utils
Priority: optional
Installed-Size: $(du -sk "$STAGE/usr" | cut -f1)
Homepage: https://github.com/ssk101/DroidLeap
Description: Use an Android device as a screen-edge extension over USB
 Moving the mouse off a chosen desktop edge hands mouse and keyboard to an
 Android phone or tablet connected over USB (Android Open Accessory HID).
 Pause, or moving back through the device's edge (with the DroidLeap Android
 app), hands them back. Runs in the system tray. Needs a Wayland desktop with
 the InputCapture portal (GNOME 46+).
EOF

dpkg-deb --root-owner-group --build "$STAGE" "$OUT/${NAME}_${VERSION}_${ARCH}.deb"
