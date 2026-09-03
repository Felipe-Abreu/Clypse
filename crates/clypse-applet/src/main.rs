/// Miniaplicativo (applet) do Clypse para o painel do COSMIC.
///
/// Ícone no painel → popup com o histórico recente da área de transferência,
/// busca instantânea e atalho para abrir a janela completa (clypse-gui).
/// Fala com o clypse-daemon pelo mesmo socket IPC da GUI.
mod ipc;

use cosmic::app::{Core, Task};
use cosmic::iced::window::Id;
use cosmic::iced::{Length, Limits, Rectangle, Vector};
use cosmic::surface::action::{app_popup, destroy_popup};
use cosmic::widget;
use cosmic::Element;
use ipc::ClipItem;

const APP_ID: &str = "io.github.felipe_abreu.Clypse";
const MAX_PREVIEW_CHARS: usize = 64;

fn main() -> cosmic::iced::Result {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("CLYPSE_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    cosmic::applet::run::<ClypseApplet>(())
}

#[derive(Default)]
struct ClypseApplet {
    core:    Core,
    popup:   Option<Id>,
    search:  String,
    items:   Vec<ClipItem>,
    loading: bool,
}

#[derive(Debug, Clone)]
enum Message {
    PanelPressed(Vector, Rectangle),
    PopupClosed(Id),
    SearchChanged(String),
    ItemsLoaded(Vec<ClipItem>),
    CopyText(String),
    OpenGui,
}

impl cosmic::Application for ClypseApplet {
    type Executor = cosmic::executor::Default;
    type Flags = ();
    type Message = Message;
    const APP_ID: &'static str = APP_ID;

    fn core(&self) -> &Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut Core {
        &mut self.core
    }

    fn init(core: Core, _flags: ()) -> (Self, Task<Message>) {
        (Self { core, ..Default::default() }, Task::none())
    }

    fn on_close_requested(&self, id: Id) -> Option<Message> {
        Some(Message::PopupClosed(id))
    }

    fn style(&self) -> Option<cosmic::iced::theme::Style> {
        Some(cosmic::applet::style())
    }

    // Ícone no painel
    fn view(&self) -> Element<'_, Message> {
        self.core
            .applet
            .icon_button(APP_ID)
            .on_press_with_rectangle(Message::PanelPressed)
            .into()
    }

    fn view_window(&self, _id: Id) -> Element<'_, Message> {
        // Popups são renderizados pela closure do app_popup; isto é fallback.
        widget::text::body("").into()
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::PanelPressed(offset, bounds) => {
                if let Some(id) = self.popup.take() {
                    return surface_task(destroy_popup(id));
                }
                self.search.clear();
                self.items.clear();
                self.loading = true;

                let action = app_popup::<ClypseApplet>(
                    |_| Default::default(),
                    move |state: &mut ClypseApplet| {
                        let new_id = Id::unique();
                        state.popup = Some(new_id);
                        let mut popup_settings = state.core.applet.get_popup_settings(
                            state.core.main_window_id().unwrap(),
                            new_id,
                            None,
                            None,
                            None,
                        );
                        popup_settings.positioner.anchor_rect = Rectangle {
                            x:      (bounds.x - offset.x) as i32,
                            y:      (bounds.y - offset.y) as i32,
                            width:  bounds.width as i32,
                            height: bounds.height as i32,
                        };
                        popup_settings.positioner.size_limits = Limits::NONE
                            .min_width(320.0)
                            .max_width(420.0)
                            .min_height(200.0)
                            .max_height(560.0);
                        popup_settings
                    },
                    Some(Box::new(|state: &ClypseApplet| {
                        Element::from(state.core.applet.popup_container(state.popup_view()))
                            .map(cosmic::Action::App)
                    })),
                );

                return Task::batch(vec![surface_task(action), load_items(None)]);
            }
            Message::PopupClosed(id) => {
                if self.popup == Some(id) {
                    self.popup = None;
                }
            }
            Message::SearchChanged(s) => {
                self.search = s.clone();
                self.loading = true;
                let query = if s.is_empty() { None } else { Some(s) };
                return load_items(query);
            }
            Message::ItemsLoaded(items) => {
                self.items = items;
                self.loading = false;
            }
            Message::CopyText(content) => {
                let close = match self.popup.take() {
                    Some(id) => surface_task(destroy_popup(id)),
                    None     => Task::none(),
                };
                return Task::batch(vec![
                    cosmic::iced::clipboard::write::<cosmic::Action<Message>>(content),
                    close,
                ]);
            }
            Message::OpenGui => {
                let _ = std::process::Command::new("clypse")
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn();
                if let Some(id) = self.popup.take() {
                    return surface_task(destroy_popup(id));
                }
            }
        }
        Task::none()
    }
}

impl ClypseApplet {
    // Conteúdo do popup: busca + histórico + atalho para a GUI completa
    fn popup_view(&self) -> Element<'_, Message> {
        let search = widget::search_input("Search clipboard…", &self.search)
            .on_input(Message::SearchChanged)
            .width(Length::Fill);

        let mut rows: Vec<Element<'_, Message>> = Vec::with_capacity(self.items.len().max(1));

        if self.items.is_empty() {
            let hint = if self.loading {
                "Loading…"
            } else if self.search.is_empty() {
                "Clipboard history is empty"
            } else {
                "No results"
            };
            rows.push(
                widget::container(widget::text::body(hint))
                    .width(Length::Fill)
                    .padding(12)
                    .into(),
            );
        } else {
            for item in &self.items {
                rows.push(item_row(item));
            }
        }

        let list = widget::column(rows).spacing(2).width(Length::Fill);

        widget::column(vec![
            search.into(),
            widget::scrollable(list)
                .width(Length::Fill)
                .height(Length::Fixed(380.0))
                .into(),
            widget::divider::horizontal::default().into(),
            widget::button::text("Open Clypse")
                .width(Length::Fill)
                .on_press(Message::OpenGui)
                .into(),
        ])
        .spacing(8)
        .padding(12)
        .width(Length::Fill)
        .into()
    }
}

fn item_row(item: &ClipItem) -> Element<'_, Message> {
    let (label, action) = match &item.content {
        Some(text) if !item.mime_type.starts_with("image/") => {
            (preview(text), Message::CopyText(text.clone()))
        }
        // Imagens/binários: o applet não seta clipboard binário — abre a GUI
        _ => ("[ Image — open Clypse to copy ]".to_string(), Message::OpenGui),
    };

    let marker = if item.is_pinned {
        "📌 "
    } else if item.is_favorite {
        "★ "
    } else {
        ""
    };

    widget::button::custom(widget::text::body(format!("{}{}", marker, label)))
        .width(Length::Fill)
        .class(cosmic::theme::Button::MenuItem)
        .on_press(action)
        .into()
}

fn preview(text: &str) -> String {
    let line = text.trim().lines().next().unwrap_or("").trim();
    let mut out: String = line.chars().take(MAX_PREVIEW_CHARS).collect();
    if line.chars().count() > MAX_PREVIEW_CHARS {
        out.push('…');
    }
    out
}

fn surface_task(action: cosmic::surface::Action) -> Task<Message> {
    cosmic::task::message(cosmic::Action::Cosmic(cosmic::app::Action::Surface(action)))
}

fn load_items(search: Option<String>) -> Task<Message> {
    cosmic::task::future(async move { Message::ItemsLoaded(ipc::fetch_items(search).await) })
}
