# syntax=docker/dockerfile:1
#
# APPricot spike image: the dev image plus what the pilot application needs at run time, for
# the M1 capture spike (docs/spike/README.md). Built and run only through compose's `spike`
# profile:
#
#   docker compose --profile spike build spike
#   docker compose --profile spike run --rm spike just spike-selftest
#
# `FROM dev` is the dev service's image: compose.yml passes it as the additional build context
# `dev` (`service:dev`), so compose builds the dev image first and this one on top of it. The
# toolchain, Xvfb, the entrypoint, the user and every volume are the dev image's; this layer
# adds shared libraries and nothing else.
#
# The pilot application itself is NOT in the image. It is bind-mounted read-only at /pilot
# from APPRICOT_PILOT_DIR (a path in the gitignored .env), so no binary of a third party is
# built into anything here, and the image stays generic: any Qt6/xcb application runs in it.
#
# Dev only, like the dev image: never pushed, never conveyed. The libraries below are Debian's
# and are not part of the linked graph that docs/adr/0002-licence.md gates.

FROM dev

USER root

ARG DEBIAN_MIRROR=deb.debian.org

# What a Qt 6 application on the xcb platform plugin loads (Qt's "X11 requirements",
# https://doc.qt.io/qt-6/linux-requirements.html, checked 2026-09-23), for a build that links
# Qt statically and xcb dynamically:
#
# - libxcb-* and libxkbcommon-x11-0: the xcb platform plugin's X extensions and keyboard. The
#   page's table also lists libX11, libX11-xcb, libXext, libXrender, libSM, libICE and GLib.
# - libegl1, libgl1, libegl-mesa0, libgl1-mesa-dri: EGL/GL through Mesa's software rasteriser,
#   so an application that asks for OpenGL gets llvmpipe instead of failing to start. The
#   spike records which scene-graph backend the application chose (QSG_INFO).
# - libfontconfig1, libfreetype6: text. fonts-dejavu-core (in the dev image) covers Greek.
# - libdbus-1-3: Qt's accessibility and platform-theme lookups, which fail soft without a bus.
# - procps: ps and pmap, to look at a running session by hand.
#
# The list was checked against `ldd` of the pilot's binary inside this image (no line reading
# "not found"); `scripts/spike/run.sh` repeats that check on every run and writes it into the
# run's meta.md.
# hadolint ignore=DL3008
RUN --mount=type=cache,target=/var/cache/apt,sharing=locked \
    --mount=type=cache,target=/var/lib/apt/lists,sharing=locked \
    sed -i "s|//deb.debian.org/|//${DEBIAN_MIRROR}/|g" /etc/apt/sources.list.d/debian.sources \
 && apt-get update \
 && apt-get install -y --no-install-recommends \
      libxkbcommon0 libxkbcommon-x11-0 \
      libxcb1 libxcb-cursor0 libxcb-glx0 libxcb-icccm4 libxcb-image0 libxcb-keysyms1 \
      libxcb-randr0 libxcb-render0 libxcb-render-util0 libxcb-shape0 libxcb-shm0 \
      libxcb-sync1 libxcb-util1 libxcb-xfixes0 libxcb-xinerama0 libxcb-xinput0 libxcb-xkb1 \
      libegl1 libgl1 libegl-mesa0 libgl1-mesa-dri \
      libx11-6 libx11-xcb1 libxext6 libxrender1 libsm6 libice6 libglib2.0-0 \
      libfontconfig1 libfreetype6 libdbus-1-3 \
      procps

# A UTF-8 locale, so the application's text input and its window titles are not ASCII-folded.
# C.UTF-8 is built into Debian's libc; no locale package is needed.
ENV LANG=C.UTF-8 \
    LC_ALL=C.UTF-8

USER dev
