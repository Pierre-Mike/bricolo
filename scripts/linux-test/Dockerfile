# The Linux test image for `just linux-test`: the pinned toolchain, nextest,
# and a desktop to test against (Xvfb, openbox, dunst and gnome-keyring on a
# D-Bus session).
FROM rust:1-bookworm

RUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        build-essential pkg-config ca-certificates curl lsof procps python3 \
        xvfb xauth openbox xterm xdotool xclip x11-utils x11-xserver-utils \
        dbus dbus-x11 dunst libnotify-bin gnome-keyring libsecret-tools \
        xdg-utils libglib2.0-bin \
        espeak-ng alsa-utils \
        at-spi2-core libatk-adaptor zenity \
    && rm -rf /var/lib/apt/lists/*

# A copy drops the file capability (cap_ipc_lock) the package may set on
# gnome-keyring-daemon: Docker's default capability set lacks it, and the
# kernel then refuses to run the binary.
RUN cp /usr/bin/gnome-keyring-daemon /tmp/gkd && mv /tmp/gkd /usr/bin/gnome-keyring-daemon

# rustup reads rust-toolchain.toml: Rust 1.99.1, clippy, rustfmt and
# wasm32-wasip2.
COPY rust-toolchain.toml /tmp/toolchain/
RUN cd /tmp/toolchain && rustup toolchain install && rustup default "$(sed -n 's/^channel = "\(.*\)"/\1/p' rust-toolchain.toml)"

RUN case "$(uname -m)" in aarch64) n=linux-arm ;; *) n=linux ;; esac \
    && curl -LsSf "https://get.nexte.st/latest/$n" | tar zxf - -C /usr/local/cargo/bin

WORKDIR /src
