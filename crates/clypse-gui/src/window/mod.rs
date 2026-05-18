use crate::ipc_client::{ClipItem, DaemonMessage, GuiCommand};
use gtk4::glib;
use gtk4::gio;
use gtk4::prelude::*;
use gtk4::{Box as GtkBox, Label, ListBox, Orientation, ScrolledWindow, SearchEntry};
use libadwaita as adw;
use libadwaita::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::mpsc::SyncSender;
use tracing::warn;

// ─── Estado da janela ─────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct ClypseWindow {
    pub window:     adw::Window,
    cmd_tx:         SyncSender<GuiCommand>,
    items:          Rc<RefCell<Vec<ClipItem>>>,
    list_box:       ListBox,
    search_entry:   SearchEntry,
    status_label:   Label,
    toast_overlay:  adw::ToastOverlay,
    favorites_only: Rc<Cell<bool>>,
}

impl ClypseWindow {
    pub fn new(app: &adw::Application, cmd_tx: SyncSender<GuiCommand>) -> Self {
        // adw::Window (não ApplicationWindow) para que o Wayland app_id não coincida
        // com o nome do .desktop file — impede que a janela apareça na dock do COSMIC.
        // O ciclo de vida ainda é gerenciado pelo GApplication via set_application().
        let window = adw::Window::builder()
            .title("Clypse")
            .default_width(460)
            .default_height(600)
            .build();
        window.set_application(Some(app));

        // ── Layout principal ──────────────────────────────────────────────────
        let toolbar_view = adw::ToolbarView::new();

        let header = adw::HeaderBar::new();
        header.set_show_end_title_buttons(true);

        // Filtro de favoritos (header esquerda)
        let fav_filter_btn = gtk4::ToggleButton::new();
        fav_filter_btn.set_icon_name("starred-symbolic");
        fav_filter_btn.set_tooltip_text(Some("Show favorites only"));
        fav_filter_btn.add_css_class("flat");
        header.pack_start(&fav_filter_btn);

        // Botão limpar histórico — ao lado do filtro, longe dos botões de janela
        let clear_btn = gtk4::Button::from_icon_name("edit-delete-symbolic");
        clear_btn.set_tooltip_text(Some("Clear history"));
        clear_btn.add_css_class("flat");
        header.pack_start(&clear_btn);

        // Menu de aplicação
        let menu_btn = gtk4::MenuButton::new();
        menu_btn.set_icon_name("open-menu-symbolic");
        menu_btn.add_css_class("flat");
        header.pack_end(&menu_btn);

        toolbar_view.add_top_bar(&header);

        // ── Conteúdo ──────────────────────────────────────────────────────────
        let content = GtkBox::new(Orientation::Vertical, 0);

        // Campo de busca
        let search_entry = SearchEntry::new();
        search_entry.set_placeholder_text(Some("Search clipboard history…"));
        search_entry.set_margin_start(12);
        search_entry.set_margin_end(12);
        search_entry.set_margin_top(8);
        search_entry.set_margin_bottom(8);
        content.append(&search_entry);

        content.append(&gtk4::Separator::new(Orientation::Horizontal));

        // Lista principal
        let list_box = ListBox::new();
        list_box.set_selection_mode(gtk4::SelectionMode::Single);
        list_box.add_css_class("boxed-list");
        list_box.set_vexpand(true);

        // Placeholder quando lista vazia
        let ph = build_placeholder();
        list_box.set_placeholder(Some(&ph));

        let scrolled = ScrolledWindow::new();
        scrolled.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Automatic);
        scrolled.set_vexpand(true);
        scrolled.set_margin_start(12);
        scrolled.set_margin_end(12);
        scrolled.set_margin_bottom(4);
        scrolled.set_child(Some(&list_box));
        content.append(&scrolled);

        // Barra de status (rodapé)
        let status_label = Label::new(Some("Connecting to daemon…"));
        status_label.add_css_class("caption");
        status_label.add_css_class("dim-label");
        status_label.set_margin_top(4);
        status_label.set_margin_bottom(8);
        content.append(&status_label);

        // ToastOverlay envolve o conteúdo inteiro
        let toast_overlay = adw::ToastOverlay::new();
        toast_overlay.set_child(Some(&content));
        toolbar_view.set_content(Some(&toast_overlay));
        window.set_content(Some(&toolbar_view));

        // Menu dropdown
        let menu = gio::Menu::new();
        menu.append(Some("Preferences"),  Some("app.preferences"));
        menu.append(Some("About Clypse"), Some("app.about"));
        menu.append(Some("Quit"),         Some("app.quit"));
        menu_btn.set_menu_model(Some(&menu));

        // ESC fecha a janela — dois caminhos:
        // 1. SearchEntry com foco: emite stop-search antes de propagar para a janela
        // 2. Qualquer outro widget com foco: capturado pelo EventControllerKey da janela
        {
            let w = window.clone();
            search_entry.connect_stop_search(move |_| {
                w.set_visible(false);
            });
        }
        {
            let w = window.clone();
            let ctrl = gtk4::EventControllerKey::new();
            ctrl.connect_key_pressed(move |_, key, _, _| {
                if key == gtk4::gdk::Key::Escape {
                    w.set_visible(false);
                    return glib::Propagation::Stop;
                }
                glib::Propagation::Proceed
            });
            window.add_controller(ctrl);
        }

        let favorites_only = Rc::new(Cell::new(false));

        let win = Self {
            window,
            cmd_tx,
            items: Rc::new(RefCell::new(Vec::new())),
            list_box,
            search_entry,
            status_label,
            toast_overlay,
            favorites_only,
        };

        win.connect_search();
        win.connect_list_activate();
        win.connect_clear_button(clear_btn);
        win.connect_favorites_filter(fav_filter_btn);

        win
    }

    // ── API pública ───────────────────────────────────────────────────────────

    /// Alterna visibilidade, recarregando itens quando abre.
    pub fn show_at_cursor(&self) {
        if self.window.is_visible() {
            self.window.set_visible(false);
            return;
        }
        let _ = self.cmd_tx.try_send(GuiCommand::GetItems {
            search: None,
            limit:  100,
            offset: 0,
        });
        self.search_entry.set_text("");
        self.window.present();
        self.search_entry.grab_focus();
    }

    // ── Mensagens do daemon ───────────────────────────────────────────────────

    pub fn handle_daemon_message(&self, msg: DaemonMessage) {
        match msg {
            DaemonMessage::Items { items, total } => {
                *self.items.borrow_mut() = items.clone();
                self.rebuild_list(&items);
                self.update_status(total, &items);
            }
            DaemonMessage::ItemAdded(item) => {
                self.items.borrow_mut().insert(0, item);
                let items = self.items.borrow().clone();
                let total = items.len() as i64;
                self.rebuild_list(&items);
                self.update_status(total, &items);
            }
            DaemonMessage::ItemDeleted { item_id } => {
                self.items.borrow_mut().retain(|i| i.id != item_id);
                let items = self.items.borrow().clone();
                let total = items.len() as i64;
                self.rebuild_list(&items);
                self.update_status(total, &items);
            }
            DaemonMessage::HistoryCleared => {
                self.items.borrow_mut().clear();
                self.rebuild_list(&[]);
                self.status_label.set_text("History cleared");
                self.show_toast("History cleared");
            }
            DaemonMessage::Disconnected => {
                self.status_label.set_text("⚠ Daemon disconnected. Reconnecting…");
            }
            DaemonMessage::Error(e) => {
                warn!("Daemon error: {}", e);
                self.status_label.set_text(&format!("Error: {}", e));
            }
            DaemonMessage::FavoriteToggled { item_id, is_favorite } => {
                if let Some(item) = self.items.borrow_mut().iter_mut().find(|i| i.id == item_id) {
                    item.is_favorite = is_favorite;
                }
                let items = self.items.borrow().clone();
                let total = items.len() as i64;
                self.rebuild_list(&items);
                self.update_status(total, &items);
            }
            DaemonMessage::PinnedToggled { item_id, is_pinned } => {
                if let Some(item) = self.items.borrow_mut().iter_mut().find(|i| i.id == item_id) {
                    item.is_pinned = is_pinned;
                }
                let items = self.items.borrow().clone();
                let total = items.len() as i64;
                self.rebuild_list(&items);
                self.update_status(total, &items);
            }
            DaemonMessage::ItemUpdated => {
                let search = current_search(&self.search_entry);
                let _ = self.cmd_tx.try_send(GuiCommand::GetItems {
                    search,
                    limit:  100,
                    offset: 0,
                });
            }
        }
    }

    // ── Internos ──────────────────────────────────────────────────────────────

    fn show_toast(&self, msg: &str) {
        let toast = adw::Toast::new(msg);
        toast.set_timeout(2);
        self.toast_overlay.add_toast(toast);
    }

    fn update_status(&self, total: i64, items: &[ClipItem]) {
        let fav_count = items.iter().filter(|i| i.is_favorite).count();
        if self.favorites_only.get() {
            self.status_label.set_text(&format!("{} favorites", fav_count));
        } else {
            self.status_label.set_text(&format!(
                "{} items{}",
                total,
                if fav_count > 0 { format!(" · {} ★", fav_count) } else { String::new() }
            ));
        }
    }

    fn rebuild_list(&self, items: &[ClipItem]) {
        while let Some(child) = self.list_box.first_child() {
            self.list_box.remove(&child);
        }
        let show_favs = self.favorites_only.get();
        for item in items {
            if show_favs && !item.is_favorite { continue; }
            let row = build_row(item, &self.cmd_tx);
            self.list_box.append(&row);
        }
    }

    // ── Sinais ────────────────────────────────────────────────────────────────

    fn connect_search(&self) {
        let cmd_tx = self.cmd_tx.clone();
        self.search_entry.connect_search_changed(move |entry| {
            let search = current_search(entry);
            let _ = cmd_tx.try_send(GuiCommand::GetItems { search, limit: 100, offset: 0 });
        });
    }

    fn connect_list_activate(&self) {
        let window        = self.window.clone();
        let items         = self.items.clone();
        let toast_overlay = self.toast_overlay.clone();

        self.list_box.connect_row_activated(move |_, row| {
            let item_id: i64 = unsafe {
                *row.data::<i64>("clip-item-id")
                    .map(|p| p.as_ref())
                    .unwrap_or(&0)
            };
            if item_id == 0 { return; }

            // Extrai todos os campos necessários do estado local
            let (content, blob_path, mime_type) = {
                let guard = items.borrow();
                match guard.iter().find(|i| i.id == item_id) {
                    Some(item) => (item.content.clone(), item.blob_path.clone(), item.mime_type.clone()),
                    None => return,
                }
            };

            tracing::debug!("Activated item id={} mime={}", item_id, mime_type);

            match content {
                Some(text) => {
                    // ── Texto: escreve no clipboard e dispara auto-paste ──────────
                    if let Some(display) = gtk4::gdk::Display::default() {
                        display.clipboard().set_text(&text);
                    }
                    let toast = adw::Toast::new("Copied — press Ctrl+V to paste");
                    toast.set_timeout(2);
                    toast_overlay.add_toast(toast);

                    let win_clone = window.clone();
                    glib::timeout_add_local_once(
                        std::time::Duration::from_millis(120),
                        move || { win_clone.set_visible(false); },
                    );
                    std::thread::spawn(move || {
                        std::thread::sleep(std::time::Duration::from_millis(600));
                        try_auto_paste();
                    });
                }

                None => {
                    // ── Imagem/binário: lê o arquivo e define o ContentProvider ───
                    if let Some(path) = blob_path {
                        match std::fs::read(&path) {
                            Ok(bytes) => {
                                let glib_bytes = glib::Bytes::from_owned(bytes);
                                let provider = gtk4::gdk::ContentProvider::for_bytes(
                                    &mime_type,
                                    &glib_bytes,
                                );
                                if let Some(display) = gtk4::gdk::Display::default() {
                                    let _ = display.clipboard().set_content(Some(&provider));
                                }
                                let toast = adw::Toast::new("Image copied to clipboard");
                                toast.set_timeout(2);
                                toast_overlay.add_toast(toast);
                                let win_clone = window.clone();
                                glib::timeout_add_local_once(
                                    std::time::Duration::from_millis(120),
                                    move || { win_clone.set_visible(false); },
                                );
                            }
                            Err(e) => {
                                tracing::warn!("Cannot read image blob {}: {}", path, e);
                                let toast = adw::Toast::new("Image file missing — recopy to restore");
                                toast.set_timeout(3);
                                toast_overlay.add_toast(toast);
                            }
                        }
                    } else {
                        let toast = adw::Toast::new("Binary content — cannot copy");
                        toast.set_timeout(2);
                        toast_overlay.add_toast(toast);
                    }
                }
            }
        });
    }

    fn connect_clear_button(&self, btn: gtk4::Button) {
        let cmd_tx = self.cmd_tx.clone();
        btn.connect_clicked(move |_| {
            let _ = cmd_tx.try_send(GuiCommand::ClearHistory);
        });
    }

    fn connect_favorites_filter(&self, btn: gtk4::ToggleButton) {
        let favorites_only = self.favorites_only.clone();
        let items          = self.items.clone();
        let this           = self.clone();

        btn.connect_toggled(move |b| {
            favorites_only.set(b.is_active());
            let items = items.borrow().clone();
            let total = items.len() as i64;
            this.rebuild_list(&items);
            this.update_status(total, &items);
        });
    }
}

// ─── Auto-paste best-effort ───────────────────────────────────────────────────

/// Tenta simular Ctrl+V usando ferramentas disponíveis no sistema.
/// - wtype (Wayland nativo): apt install wtype
/// - xdotool (X11/XWayland): apt install xdotool
/// Falha silenciosamente se nenhuma estiver disponível.
fn try_auto_paste() {
    // wtype: virtual-keyboard-v1 Wayland protocol
    // Sintaxe: -M (hold modifier), -k (press key), -m (release modifier)
    if std::process::Command::new("wtype")
        .args(["-M", "ctrl", "-k", "v", "-m", "ctrl"])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
    {
        return;
    }

    // ydotool: uinput fallback (não precisa de protocolo do compositor)
    if std::process::Command::new("ydotool")
        .args(["key", "29:1", "47:1", "47:0", "29:0"])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
    {
        return;
    }

    // xdotool: X11 / XWayland fallback
    let _ = std::process::Command::new("xdotool")
        .args(["key", "--clearmodifiers", "ctrl+v"])
        .status();
}

// ─── Widgets de linha ─────────────────────────────────────────────────────────

fn build_row(item: &ClipItem, cmd_tx: &SyncSender<GuiCommand>) -> adw::ActionRow {
    let row = adw::ActionRow::new();
    row.set_activatable(true);

    // Prefix: thumbnail para imagens, ícone MIME para texto
    if item.mime_type.starts_with("image/") {
        if let Some(ref path) = item.blob_path {
            let pic = gtk4::Picture::for_filename(path);
            pic.set_can_shrink(true);
            pic.set_content_fit(gtk4::ContentFit::Cover);
            pic.set_size_request(48, 48);
            row.add_prefix(&pic);
        } else {
            let icon = gtk4::Image::from_icon_name("image-x-generic-symbolic");
            icon.add_css_class("dim-label");
            row.add_prefix(&icon);
        }
    } else {
        let icon = gtk4::Image::from_icon_name(mime_to_icon(&item.mime_type));
        icon.add_css_class("dim-label");
        row.add_prefix(&icon);
    }

    // Preview — imagens mostram tipo, texto mostra primeira linha truncada em 80 chars
    let preview = if item.mime_type.starts_with("image/") {
        friendly_mime(&item.mime_type).to_string()
    } else {
        let raw        = item.content.as_deref().unwrap_or("[binary]");
        let first_line = raw.lines().next().unwrap_or("").trim();
        truncate_chars(first_line, 80)
    };
    row.set_title(&glib::markup_escape_text(&preview));
    row.set_subtitle(&format!(
        "{} · {}",
        friendly_mime(&item.mime_type),
        format_size(item.byte_size)
    ));

    // Badge de pin (decorativo, suffix)
    if item.is_pinned {
        let pin = gtk4::Image::from_icon_name("view-pin-symbolic");
        pin.set_tooltip_text(Some("Pinned — won't be auto-deleted"));
        pin.set_valign(gtk4::Align::Center);
        pin.add_css_class("dim-label");
        row.add_suffix(&pin);
    }

    // Botão de favorito (interativo, suffix)
    let fav_icon = if item.is_favorite { "starred-symbolic" } else { "non-starred-symbolic" };
    let fav_btn  = gtk4::Button::from_icon_name(fav_icon);
    fav_btn.add_css_class("flat");
    fav_btn.set_valign(gtk4::Align::Center);
    fav_btn.set_tooltip_text(Some(if item.is_favorite {
        "Remove from favorites"
    } else {
        "Add to favorites — never auto-deleted"
    }));

    let item_id = item.id;
    let tx      = cmd_tx.clone();
    fav_btn.connect_clicked(move |_| {
        let _ = tx.try_send(GuiCommand::ToggleFavorite(item_id));
    });
    row.add_suffix(&fav_btn);

    // Botão de deletar
    let del_btn = gtk4::Button::from_icon_name("user-trash-symbolic");
    del_btn.add_css_class("flat");
    del_btn.add_css_class("destructive-action");
    del_btn.set_valign(gtk4::Align::Center);
    del_btn.set_tooltip_text(Some("Delete item"));
    del_btn.set_opacity(0.0);

    let del_tx = cmd_tx.clone();
    del_btn.connect_clicked(move |_| {
        let _ = del_tx.try_send(GuiCommand::DeleteItem(item_id));
    });
    row.add_suffix(&del_btn);

    // Mostra o botão de deletar ao passar o mouse
    {
        let d = del_btn.clone();
        let hover = gtk4::EventControllerMotion::new();
        hover.connect_enter(move |_, _, _| { d.set_opacity(1.0); });
        let d2 = del_btn.clone();
        hover.connect_leave(move |_| { d2.set_opacity(0.0); });
        row.add_controller(hover);
    }

    // Item_id armazenado no widget para recuperação na ativação
    unsafe { row.set_data("clip-item-id", item.id) };

    row
}

fn build_placeholder() -> GtkBox {
    let ph = GtkBox::new(Orientation::Vertical, 12);
    ph.set_valign(gtk4::Align::Center);
    ph.set_vexpand(true);

    let icon = gtk4::Image::from_icon_name("edit-copy-symbolic");
    icon.set_pixel_size(48);
    icon.add_css_class("dim-label");

    let title = Label::new(Some("No clipboard history yet"));
    title.add_css_class("dim-label");
    title.add_css_class("title-4");

    let sub = Label::new(Some("Copy something to get started"));
    sub.add_css_class("dim-label");
    sub.add_css_class("caption");

    ph.append(&icon);
    ph.append(&title);
    ph.append(&sub);
    ph
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

fn current_search(entry: &SearchEntry) -> Option<String> {
    let text = entry.text();
    if text.is_empty() { None } else { Some(text.to_string()) }
}

fn truncate_chars(s: &str, max: usize) -> String {
    let mut count = 0;
    let mut end   = s.len();
    for (i, _) in s.char_indices() {
        if count == max { end = i; break; }
        count += 1;
    }
    if end < s.len() { format!("{}…", &s[..end]) } else { s.to_string() }
}

fn mime_to_icon(mime: &str) -> &'static str {
    if mime.starts_with("image/")   { "image-x-generic-symbolic" }
    else if mime == "text/uri-list" { "folder-symbolic" }
    else if mime == "text/uri"      { "web-browser-symbolic" }
    else if mime.contains("json")   { "text-x-script-symbolic" }
    else if mime == "text/html"     { "text-html-symbolic" }
    else                            { "edit-copy-symbolic" }
}

fn friendly_mime(mime: &str) -> &str {
    match mime {
        "text/plain" | "text/plain;charset=utf-8" => "Text",
        "text/uri"                                => "Link",
        "text/uri-list"                           => "Files",
        "text/html"                               => "HTML",
        "text/json" | "application/json"          => "JSON",
        "image/png"                               => "PNG",
        "image/jpeg"                              => "JPEG",
        "image/webp"                              => "WebP",
        other                                     => other,
    }
}

fn format_size(bytes: i64) -> String {
    const KB: i64 = 1024;
    const MB: i64 = KB * 1024;
    if bytes < KB      { format!("{} B", bytes) }
    else if bytes < MB { format!("{:.1} KB", bytes as f64 / KB as f64) }
    else               { format!("{:.1} MB", bytes as f64 / MB as f64) }
}
