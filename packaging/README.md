# Packaging

The build is Cargo-only. Qt 6 is linked from the system unless a bundler
supplies it.

## AppImage

```sh
packaging/build-appimage.sh
```

Needs `linuxdeploy` and `linuxdeploy-plugin-qt` on `PATH` (or `LINUXDEPLOY=`).
The Qt plugin bundles Qt libraries and QML modules, including QtWebEngine and
QtQuick3D.

## Debian

```sh
packaging/build-deb.sh
```

Produces `target/corcho_<version>_<arch>.deb`, depending on the distribution's
Qt 6 packages.

## Flatpak

```sh
flatpak-cargo-generator Cargo.lock -o packaging/flatpak/cargo-sources.json
flatpak-builder --user --install --force-clean build packaging/flatpak/io.corcho.Corcho.yml
```

The manifest uses `org.kde.Platform//6.8` with QtWebEngine available.

## Desktop entry and protocol

`corcho.desktop` declares `x-scheme-handler/corcho`. Install it with the binary
and the icon, then register the handler:

```sh
update-desktop-database ~/.local/share/applications
xdg-mime default corcho.desktop x-scheme-handler/corcho
```

Deep links: `corcho://grid/<id>` and `corcho://card/<id>`.

## Private relay

[`iroh-relay/`](iroh-relay/) contains a hardened generic `iroh-relay` behind an
existing Traefik instance, plus a small access-request service. Users request
access from the peer-sync wizard; the operator approves endpoint IDs over SSH.

## Encrypted 48h cache

[`iroh-cache/`](iroh-cache/) builds a container image of the read-only sync
daemon and runs it with `--ttl-hours 48` and a cache ticket. The cache only
holds ciphertext.
