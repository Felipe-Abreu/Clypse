PREFIX     ?= /usr/local
BINDIR     := $(PREFIX)/bin
DATADIR    := $(PREFIX)/share
SYSTEMD_USER_DIR := $(HOME)/.config/systemd/user

CARGO      := $(shell which cargo 2>/dev/null || echo $(HOME)/.cargo/bin/cargo)
RELEASE    := --release
PROFILE    := release
TARGET_DIR := target/$(PROFILE)

.PHONY: all build debug install uninstall service enable disable \
        daemon-bin gui-bin check fmt clippy test clean deb

# ─── Build ────────────────────────────────────────────────────────────────────

all: build

build:
	$(CARGO) build $(RELEASE)

debug:
	$(CARGO) build

daemon-bin:
	$(CARGO) build $(RELEASE) -p clypse-daemon

gui-bin:
	$(CARGO) build $(RELEASE) -p clypse-gui

# ─── Qualidade ────────────────────────────────────────────────────────────────

check:
	$(CARGO) check --all

fmt:
	$(CARGO) fmt --all

clippy:
	$(CARGO) clippy --all -- -D warnings

test:
	$(CARGO) test --all

# ─── Instalação local (sem root) ─────────────────────────────────────────────

dev-install: build
	@mkdir -p $(HOME)/.local/bin
	@mkdir -p $(HOME)/.local/share/applications
	@mkdir -p $(HOME)/.local/share/icons/hicolor/scalable/apps
	install -m755 $(TARGET_DIR)/clypse-daemon $(HOME)/.local/bin/clypse-daemon
	install -m755 $(TARGET_DIR)/clypse        $(HOME)/.local/bin/clypse
	install -m644 data/io.github.clypse.Clypse.svg \
	    $(HOME)/.local/share/icons/hicolor/scalable/apps/io.github.clypse.Clypse.svg
	install -m644 data/io.github.clypse.Clypse.desktop \
	    $(HOME)/.local/share/applications/io.github.clypse.Clypse.desktop
	-gtk-update-icon-cache -f -t $(HOME)/.local/share/icons/hicolor 2>/dev/null
	-update-desktop-database $(HOME)/.local/share/applications 2>/dev/null
	$(MAKE) service
	@echo ""
	@echo "✓ Clypse installed to ~/.local/bin/"
	@echo "  Run 'make enable' to start the daemon service."
	@echo "  Then run 'clypse' to open the interface."

dev-uninstall:
	$(MAKE) disable
	rm -f $(HOME)/.local/bin/clypse-daemon
	rm -f $(HOME)/.local/bin/clypse
	rm -f $(SYSTEMD_USER_DIR)/clypse-daemon.service
	rm -f $(HOME)/.local/share/icons/hicolor/scalable/apps/io.github.clypse.Clypse.svg
	rm -f $(HOME)/.local/share/applications/io.github.clypse.Clypse.desktop
	-gtk-update-icon-cache -f -t $(HOME)/.local/share/icons/hicolor 2>/dev/null
	@echo "✓ Clypse removed."

# ─── Instalação do sistema (requer sudo) ─────────────────────────────────────

install: build
	install -Dm755 $(TARGET_DIR)/clypse-daemon  $(DESTDIR)$(BINDIR)/clypse-daemon
	install -Dm755 $(TARGET_DIR)/clypse          $(DESTDIR)$(BINDIR)/clypse
	install -Dm644 data/io.github.clypse.Clypse.desktop \
	    $(DESTDIR)$(DATADIR)/applications/io.github.clypse.Clypse.desktop
	install -Dm644 data/io.github.clypse.Clypse.metainfo.xml \
	    $(DESTDIR)$(DATADIR)/metainfo/io.github.clypse.Clypse.metainfo.xml
	install -Dm644 data/io.github.clypse.Clypse.svg \
	    $(DESTDIR)$(DATADIR)/icons/hicolor/scalable/apps/io.github.clypse.Clypse.svg
	install -Dm644 data/systemd/clypse-daemon.service \
	    $(DESTDIR)$(DATADIR)/systemd/user/clypse-daemon.service
	@echo "✓ Clypse installed. Run 'make service enable' to activate daemon."

uninstall:
	rm -f $(DESTDIR)$(BINDIR)/clypse-daemon
	rm -f $(DESTDIR)$(BINDIR)/clypse
	rm -f $(DESTDIR)$(DATADIR)/applications/io.github.clypse.Clypse.desktop
	rm -f $(DESTDIR)$(DATADIR)/metainfo/io.github.clypse.Clypse.metainfo.xml
	rm -f $(DESTDIR)$(DATADIR)/icons/hicolor/scalable/apps/io.github.clypse.Clypse.svg
	rm -f $(DESTDIR)$(DATADIR)/systemd/user/clypse-daemon.service

# ─── Serviço systemd do usuário ───────────────────────────────────────────────

service:
	@mkdir -p $(SYSTEMD_USER_DIR)
	@sed 's|%h|$(HOME)|g; s|%t|$(XDG_RUNTIME_DIR)|g' \
	    data/systemd/clypse-daemon.service > $(SYSTEMD_USER_DIR)/clypse-daemon.service
	@systemctl --user daemon-reload
	@echo "✓ systemd user service installed at $(SYSTEMD_USER_DIR)/clypse-daemon.service"

enable: service
	systemctl --user enable --now clypse-daemon.service
	@echo "✓ clypse-daemon started and enabled on login."

disable:
	systemctl --user disable --now clypse-daemon.service 2>/dev/null || true

status:
	systemctl --user status clypse-daemon.service

logs:
	journalctl --user -u clypse-daemon.service -f

restart:
	systemctl --user restart clypse-daemon.service

# ─── Pacotes ─────────────────────────────────────────────────────────────────

deb: build
	dpkg-buildpackage -us -uc -b -d

# ─── Limpeza ─────────────────────────────────────────────────────────────────

clean:
	$(CARGO) clean
	rm -f ../*.deb ../*.buildinfo ../*.changes
