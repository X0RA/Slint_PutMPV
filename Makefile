BINARY       ?= putmpv
PREFIX       ?= /usr
BINDIR       ?= $(PREFIX)/bin
DATADIR      ?= $(PREFIX)/share
APPDIR       ?= $(DATADIR)/applications
ICONDIR      ?= $(DATADIR)/icons/hicolor/256x256/apps
LICENSEDIR   ?= $(DATADIR)/licenses/$(BINARY)
CARGO        ?= cargo
REPO_ROOT    := $(dir $(abspath $(lastword $(MAKEFILE_LIST))))
DESKTOP_FILE := $(REPO_ROOT)scripts/installer/$(BINARY).desktop
ICON_FILE    := $(REPO_ROOT)ui/assets/appicon.png
LICENSE_FILE := $(REPO_ROOT)LICENSE

# Detect fast linker flags
ifeq ($(shell command -v clang >/dev/null 2>&1 && command -v mold >/dev/null 2>&1 && echo 1),1)
  LINKER_FLAGS := -C linker=clang -C link-arg=-fuse-ld=mold
else ifeq ($(shell command -v clang >/dev/null 2>&1 && command -v lld >/dev/null 2>&1 && echo 1),1)
  LINKER_FLAGS := -C linker=clang -C link-arg=-fuse-ld=lld
else
  LINKER_FLAGS :=
endif

DEV_RUSTFLAGS     := $(strip $(RUSTFLAGS) $(LINKER_FLAGS))
RELEASE_RUSTFLAGS := $(strip $(RUSTFLAGS) -C target-cpu=native)

.PHONY: dev build release run install uninstall clean

dev:
	cd "$(REPO_ROOT)" && CARGO_INCREMENTAL=1 RUSTFLAGS="$(DEV_RUSTFLAGS)" $(CARGO) run --profile dev-fast

build: release

release:
	cd "$(REPO_ROOT)" && RUSTFLAGS="$(RELEASE_RUSTFLAGS)" $(CARGO) build --release

run: release
	cd "$(REPO_ROOT)" && exec ./target/release/$(BINARY)

install: release
	install -Dm755 "$(REPO_ROOT)target/release/$(BINARY)" "$(DESTDIR)$(BINDIR)/$(BINARY)"
	install -Dm644 "$(LICENSE_FILE)" "$(DESTDIR)$(LICENSEDIR)/LICENSE"
	install -Dm644 "$(DESKTOP_FILE)" "$(DESTDIR)$(APPDIR)/$(BINARY).desktop"
	install -Dm644 "$(ICON_FILE)" "$(DESTDIR)$(ICONDIR)/$(BINARY).png"
	if [ -z "$(DESTDIR)" ] && command -v update-desktop-database >/dev/null 2>&1; then update-desktop-database "$(APPDIR)"; fi
	if [ -z "$(DESTDIR)" ] && command -v gtk-update-icon-cache >/dev/null 2>&1; then gtk-update-icon-cache -q "$(DATADIR)/icons/hicolor"; fi

uninstall:
	rm -f "$(DESTDIR)$(BINDIR)/$(BINARY)"
	rm -f "$(DESTDIR)$(LICENSEDIR)/LICENSE"
	rm -f "$(DESTDIR)$(APPDIR)/$(BINARY).desktop"
	rm -f "$(DESTDIR)$(ICONDIR)/$(BINARY).png"
	rmdir "$(DESTDIR)$(LICENSEDIR)" 2>/dev/null || true
	if [ -z "$(DESTDIR)" ] && command -v update-desktop-database >/dev/null 2>&1; then update-desktop-database "$(APPDIR)"; fi
	if [ -z "$(DESTDIR)" ] && command -v gtk-update-icon-cache >/dev/null 2>&1; then gtk-update-icon-cache -q "$(DATADIR)/icons/hicolor"; fi

clean:
	cd "$(REPO_ROOT)" && $(CARGO) clean
