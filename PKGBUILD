# Maintainer: cefege
pkgname=omarchy-media
pkgver=0.1.0
pkgrel=1
pkgdesc="Native replacements for the Omarchy display, audio and input shell commands"
arch=('aarch64' 'x86_64')
url="https://github.com/cefege/omarchy-media"
license=('MIT')
depends=()
makedepends=('cargo')
optdepends=('brightnessctl: internal panel brightness without root'
            'ddcutil: external displays over DDC/CI'
            'hyprland: monitor names and DPMS dispatch'
            'quickshell: the on-screen display')

source=("$pkgname-$pkgver.tar.gz")
sha256sums=('SKIP')

build() {
    cargo build --release --locked
}

check() {
    cargo test --release --locked
}

package() {
    install -Dm755 "target/release/$pkgname" "$pkgdir/usr/bin/$pkgname"

    # The binary dispatches on argv[0], so the command it replaces is a link.
    # Everything that calls `omarchy-brightness-display` keeps working.
    ln -s "$pkgname" "$pkgdir/usr/bin/omarchy-brightness-display"
}
