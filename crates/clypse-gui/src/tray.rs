use ksni::{menu::StandardItem, MenuItem, Tray, TrayService};
use std::sync::mpsc;

pub enum TrayAction {
    Show,
    Quit,
}

struct ClipseTray {
    tx: mpsc::SyncSender<TrayAction>,
}

impl Tray for ClipseTray {
    fn id(&self) -> String {
        "io.github.clypse.Clypse".into()
    }

    fn icon_name(&self) -> String {
        "io.github.clypse.Clypse".into()
    }

    fn title(&self) -> String {
        "Clypse".into()
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.tx.try_send(TrayAction::Show);
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        vec![
            StandardItem {
                label:    "Open Clypse".into(),
                activate: Box::new(|t: &mut Self| {
                    let _ = t.tx.try_send(TrayAction::Show);
                }),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label:    "Quit".into(),
                activate: Box::new(|t: &mut Self| {
                    let _ = t.tx.try_send(TrayAction::Quit);
                }),
                ..Default::default()
            }
            .into(),
        ]
    }
}

/// Inicia o ícone de tray em thread separada (SNI — COSMIC/GNOME/KDE panel).
/// Retorna o receiver de ações para poll na thread GTK.
pub fn start() -> mpsc::Receiver<TrayAction> {
    let (tx, rx) = mpsc::sync_channel::<TrayAction>(16);

    TrayService::new(ClipseTray { tx }).spawn();

    rx
}
