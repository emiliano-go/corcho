FROM rust:1.91-trixie

RUN apt-get update && apt-get install -y --no-install-recommends \
    qt6-base-dev \
    qt6-declarative-dev \
    qt6-svg-dev \
    qt6-webengine-dev \
    qt6-tools-dev \
    libqt6webenginequick6 \
    libqt6quickcontrols2-6 \
    libasound2-dev \
    pkg-config \
    cmake \
    ninja-build \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /src
COPY . .

ENV QMAKE=/usr/lib/qt6/bin/qmake6
RUN cargo build --release

ENV QT_QPA_PLATFORM=offscreen
USER 1001:1001
ENTRYPOINT ["/src/target/release/corcho"]
