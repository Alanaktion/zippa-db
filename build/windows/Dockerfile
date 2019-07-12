# This Dockerfile should create an environment capable of cross-compiling our
# application for Windows, but is currently untested.
FROM archlinux/base

# Install build tools + go + gtk
RUN pacman -Syu --needed base-devel go gtk3
RUN mkdir -p $HOME/aur
RUN cd $HOME/aur && git clone https://aur.archlinux.org/yay.git
RUN cd $HOME/aur/yay && makepkg -si

# Build w64 libs
RUN cd $HOME/aur && git clone https://aur.archlinux.org/mingw-w64-gcc-base.git
RUN cd $HOME/aur/mingw-w64-gcc-base && makepkg -s && pacman -U --asdeps mingw-w64-gcc-base.tar.xz
RUN cd $HOME/aur && git clone https://aur.archlinux.org/mingw-w64-cairo-bootstrap.git
RUN cd $HOME/aur/mingw-w64-cairo-bootstrap && makepkg -s && pacman -U --asdeps mingw-w64-cairo-bootstrap.tar.xz
RUN cd $HOME/aur && git clone https://aur.archlinux.org/mingw-w64-freetype2-bootstrap.git
RUN cd $HOME/aur/mingw-w64-freetype2-bootstrap && makepkg -s && pacman -U --asdeps mingw-w64-freetype2-bootstrap.tar.xz
RUN cd $HOME/aur && git clone https://aur.archlinux.org/mingw-w64-gtk3.git
RUN cd $HOME/aur/mingw-w64-gtk3 && makepkg -si

# Install/build gotk3
RUN go get github.com/gotk3/gotk3/gtk
RUN PKG_CONFIG_PATH=/usr/x86_64-w64-mingw32/lib/pkgconfig CGO_ENABLED=1 CC=x86_64-w64-mingw32-cc GOOS=windows GOARCH=amd64 go install github.com/gotk3/gotk3/gtk
