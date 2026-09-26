# Maintainer: cefege <cefege@users.noreply.github.com>
pkgname=omarchy-media
pkgver=0.1.0
pkgrel=1
pkgdesc='Native replacements for the Omarchy shell commands that run while a key is held down'
arch=('aarch64' 'x86_64')
url='https://github.com/cefege/omarchy-media'
license=('MIT')
depends=()
makedepends=('cargo')
optdepends=('brightnessctl: internal panel brightness without root'
            'ddcutil: external displays over DDC/CI'
            'hyprland: monitor names and DPMS dispatch'
            'quickshell: the on-screen display')
source=("$pkgname-$pkgver.tar.gz::https://github.com/cefege/omarchy-media/archive/v$pkgver.tar.gz")
sha256sums=('e9bf8421ee9675511856cdde0334a964dbeb8bb60ffe264c5d9a1fb6c68f0b4a')

build() {
    cd "$srcdir/$pkgname-$pkgver"
    cargo build --release --locked
}

check() {
    cd "$srcdir/$pkgname-$pkgver"
    cargo test --release --locked
}

package() {
    cd "$srcdir/$pkgname-$pkgver"
    install -Dm755 "target/release/$pkgname" "$pkgdir/usr/bin/$pkgname"

    # Only the binary. The `omarchy` package owns
    # /usr/bin/omarchy-brightness-display, and shipping a second file at that
    # path is a conflict; the wrapper that execs this one lives there instead,
    # which is also what keeps the command name working for every binding,
    # menu and script that already calls it.
}
