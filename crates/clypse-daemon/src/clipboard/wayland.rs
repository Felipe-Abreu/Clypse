/// Monitor de clipboard para sessões Wayland.
///
/// Implementa o protocolo `zwlr_data_control_manager_v1` diretamente via
/// `wayland-client` — completamente event-driven, sem polling, sem subprocessos.
///
/// Suporte de compositors:
///   ✓ wlroots-based (Sway, Hyprland, Wayfire, labwc)
///   ✓ KDE Plasma (KWin 5.24+)
///   ✓ COSMIC compositor
///   ✗ GNOME Shell (usa xdg-portal como alternativa — futuro)
///
/// Fluxo:
///   1. Conecta ao socket Wayland ($WAYLAND_DISPLAY)
///   2. Obtém globals: zwlr_data_control_manager_v1 + wl_seat
///   3. Cria data_control_device para o seat
///   4. Compositor notifica via event::DataOffer quando clipboard muda
///   5. Compositor notifica via event::Selection qual offer é o novo clipboard
///   6. Lemos o conteúdo via pipe (offer.receive(mime, write_fd))
///   7. Emitimos ClipboardEvent para o loop principal
///   8. Chamamos wl-clipboard-rs::copy para tomar ownership e garantir persistência
///
/// Resiliência: erros pontuais (pipe quebrado de um app que fechou, offer
/// inválida) não derrubam o monitor; erros de conexão disparam reconexão com
/// backoff exponencial. Só desistimos após falhas consecutivas repetidas —
/// aí o canal fecha e o main encerra o processo para o systemd reiniciar.
use super::ClipboardEvent;
use anyhow::{anyhow, Result};
use os_pipe::PipeReader;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::os::fd::AsFd;
use std::time::{Duration, Instant};
use tokio::sync::mpsc::UnboundedSender;
use tracing::{debug, error, warn};
use wayland_client::{
    event_created_child,
    globals::{registry_queue_init, GlobalListContents},
    protocol::{wl_registry, wl_seat},
    Connection, Dispatch, QueueHandle,
};
use wayland_protocols_wlr::data_control::v1::client::{
    zwlr_data_control_device_v1::{self, ZwlrDataControlDeviceV1},
    zwlr_data_control_manager_v1::ZwlrDataControlManagerV1,
    zwlr_data_control_offer_v1::{self, ZwlrDataControlOfferV1},
};

// ─── Estado do protocolo ──────────────────────────────────────────────────────

struct WaylandState {
    /// Offer recebido via data_offer mas ainda não selecionado
    pending_offer: Option<ZwlrDataControlOfferV1>,
    /// MIMEs disponíveis no offer pendente
    pending_mimes: Vec<String>,
    /// Hash do último item capturado (para deduplicação)
    last_hash: String,
    /// Canal para o loop principal do daemon
    event_tx: UnboundedSender<ClipboardEvent>,
    /// Sinaliza que um evento de selection foi recebido — para processar após dispatch
    selection_ready: Option<(ZwlrDataControlOfferV1, Vec<String>)>,
    /// Device foi invalidado pelo compositor (seat removido) — força reconexão
    finished: bool,
}

impl WaylandState {
    fn new(event_tx: UnboundedSender<ClipboardEvent>) -> Self {
        Self {
            pending_offer:   None,
            pending_mimes:   Vec::new(),
            last_hash:       String::new(),
            event_tx,
            selection_ready: None,
            finished:        false,
        }
    }

    /// Seleciona o MIME type preferido da lista de disponíveis.
    fn select_mime(mimes: &[String]) -> Option<&str> {
        let priority = [
            "text/plain;charset=utf-8",
            "text/plain",
            "image/png",
            "image/jpeg",
            "image/webp",
            "text/html",
            "text/uri-list",
            "application/json",
        ];
        for preferred in priority {
            if mimes.iter().any(|m| m == preferred) {
                return Some(preferred);
            }
        }
        // Fallback: qualquer texto disponível
        mimes.iter().find(|m| m.starts_with("text/")).map(|s| s.as_str())
    }
}

// ─── Dispatch: wl_registry ────────────────────────────────────────────────────
// registry_queue_init() gerencia o registry automaticamente via GlobalListContents.
// Não precisamos implementar Dispatch<WlRegistry> manualmente.
impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for WaylandState {
    fn event(
        _state: &mut Self,
        _registry: &wl_registry::WlRegistry,
        _event: wl_registry::Event,
        _data: &GlobalListContents,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        // registry_queue_init() cuida dos globals
    }
}

// ─── Dispatch: wl_seat ────────────────────────────────────────────────────────

impl Dispatch<wl_seat::WlSeat, ()> for WaylandState {
    fn event(
        _state: &mut Self,
        _seat: &wl_seat::WlSeat,
        _event: wl_seat::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        // Não precisamos de eventos do seat
    }
}

// ─── Dispatch: zwlr_data_control_manager_v1 ───────────────────────────────────

impl Dispatch<ZwlrDataControlManagerV1, ()> for WaylandState {
    fn event(
        _state: &mut Self,
        _manager: &ZwlrDataControlManagerV1,
        _event: <ZwlrDataControlManagerV1 as wayland_client::Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        // Manager não emite eventos
    }
}

// ─── Dispatch: zwlr_data_control_device_v1 ───────────────────────────────────

impl Dispatch<ZwlrDataControlDeviceV1, ()> for WaylandState {
    event_created_child!(WaylandState, ZwlrDataControlDeviceV1, [
        zwlr_data_control_device_v1::EVT_DATA_OFFER_OPCODE => (ZwlrDataControlOfferV1, ()),
    ]);

    fn event(
        state: &mut Self,
        _device: &ZwlrDataControlDeviceV1,
        event: zwlr_data_control_device_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            // Compositor anuncia nova data offer (antes de announcement de selection)
            zwlr_data_control_device_v1::Event::DataOffer { id } => {
                // Descarta offer anterior não utilizado
                if let Some(old) = state.pending_offer.take() {
                    old.destroy();
                }
                state.pending_offer = Some(id);
                state.pending_mimes.clear();
            }

            // Clipboard primário mudou para esta offer
            zwlr_data_control_device_v1::Event::Selection { id } => {
                // Selection anterior ainda não processada seria vazada —
                // duas selections podem chegar num mesmo dispatch
                if let Some((old, _)) = state.selection_ready.take() {
                    old.destroy();
                }
                if let Some(offer) = id {
                    // Sinaliza para processar após o dispatch completar
                    // (não fazemos I/O bloqueante dentro do handler)
                    let mimes = std::mem::take(&mut state.pending_mimes);
                    state.pending_offer = None;
                    state.selection_ready = Some((offer, mimes));
                }
            }

            // Device foi invalidado (ex: seat removido)
            zwlr_data_control_device_v1::Event::Finished => {
                error!("Wayland data control device finished");
                state.finished = true;
            }

            // Ignoramos primary selection (meio-botão do mouse)
            zwlr_data_control_device_v1::Event::PrimarySelection { .. } => {}

            _ => {}
        }
    }
}

// ─── Dispatch: zwlr_data_control_offer_v1 ────────────────────────────────────

impl Dispatch<ZwlrDataControlOfferV1, ()> for WaylandState {
    fn event(
        state: &mut Self,
        _offer: &ZwlrDataControlOfferV1,
        event: zwlr_data_control_offer_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let zwlr_data_control_offer_v1::Event::Offer { mime_type } = event {
            state.pending_mimes.push(mime_type);
        }
    }
}

// ─── Entry point ──────────────────────────────────────────────────────────────

/// Quantas falhas rápidas consecutivas toleramos antes de desistir.
/// Ao desistir, o canal fecha e o main encerra o processo — o systemd reinicia.
const MAX_CONSECUTIVE_FAILURES: u32 = 10;

pub fn run(event_tx: UnboundedSender<ClipboardEvent>, _debounce_ms: u64) -> Result<()> {
    let mut backoff_secs = 1u64;
    let mut consecutive_failures = 0u32;

    loop {
        let started = Instant::now();
        let result = run_once(&event_tx);

        match result {
            Ok(()) => return Ok(()), // encerramento limpo (daemon desligando)
            Err(e) => {
                if event_tx.is_closed() {
                    return Ok(());
                }
                // Sessão que rodou por um tempo razoável zera o backoff:
                // é uma falha nova, não a mesma repetindo
                if started.elapsed() > Duration::from_secs(60) {
                    backoff_secs = 1;
                    consecutive_failures = 0;
                }
                consecutive_failures += 1;
                if consecutive_failures >= MAX_CONSECUTIVE_FAILURES {
                    return Err(anyhow!(
                        "Wayland monitor failed {} times in a row, giving up: {:#}",
                        consecutive_failures, e
                    ));
                }
                error!(
                    "Wayland clipboard monitor error ({:#}); reconnecting in {}s",
                    e, backoff_secs
                );
                std::thread::sleep(Duration::from_secs(backoff_secs));
                backoff_secs = (backoff_secs * 2).min(30);
            }
        }
    }
}

fn run_once(event_tx: &UnboundedSender<ClipboardEvent>) -> Result<()> {
    let conn = Connection::connect_to_env()
        .map_err(|e| anyhow!("Cannot connect to Wayland: {}. Is WAYLAND_DISPLAY set?", e))?;

    let (globals, mut event_queue) = registry_queue_init::<WaylandState>(&conn)
        .map_err(|e| anyhow!("Failed to init Wayland globals: {}", e))?;

    let qh = event_queue.handle();
    let mut state = WaylandState::new(event_tx.clone());

    // Obtém globals necessários
    let manager: ZwlrDataControlManagerV1 = globals
        .bind(&qh, 1..=2, ())
        .map_err(|e| anyhow!("zwlr_data_control_manager_v1 not supported by compositor: {}. \
            This compositor may not implement wlr-data-control protocol.", e))?;

    let seat: wl_seat::WlSeat = globals
        .bind(&qh, 1..=8, ())
        .map_err(|e| anyhow!("wl_seat not available: {}", e))?;

    // Cria device para o seat
    let _device = manager.get_data_device(&seat, &qh, ());

    // Sincroniza estado inicial (limpeza do buffer de mensagens)
    event_queue.roundtrip(&mut state)
        .map_err(|e| anyhow!("Wayland roundtrip failed: {}", e))?;

    debug!("Wayland clipboard monitor ready");

    // ── Loop principal de eventos ─────────────────────────────────────────────
    // O check de selection_ready vem ANTES do dispatch: o roundtrip inicial já
    // pode ter entregue a selection do conteúdo atual do clipboard, e ela seria
    // ignorada até o próximo evento do compositor.
    loop {
        // Verifica se há uma selection pronta para processar.
        // Erro pontual (app fechou no meio da transferência, timeout) não
        // pode derrubar o monitor — só logamos e seguimos.
        if let Some((offer, mimes)) = state.selection_ready.take() {
            if let Err(e) = process_offer(offer, mimes, &mut state, &conn) {
                warn!("Failed to process clipboard offer: {:#}", e);
            }
        }

        // Compositor invalidou o device — reconecta do zero
        if state.finished {
            return Err(anyhow!("data control device finished (seat removed?)"));
        }

        // Verifica se o receiver foi dropado (daemon encerrando)
        if state.event_tx.is_closed() {
            break;
        }

        // Bloqueia até receber pelo menos um evento do compositor
        event_queue.blocking_dispatch(&mut state)
            .map_err(|e| anyhow!("Wayland dispatch error: {}", e))?;
    }

    Ok(())
}

// ─── Processamento de offer ───────────────────────────────────────────────────

fn process_offer(
    offer: ZwlrDataControlOfferV1,
    mimes: Vec<String>,
    state: &mut WaylandState,
    conn: &Connection,
) -> Result<()> {
    // Cópia anunciada por nós mesmos (take_ownership) — não há nada novo a
    // capturar, e pular a releitura evita transferir imagens de MBs à toa
    if mimes.iter().any(|m| m == CLYPSE_MARKER_MIME) {
        debug!("Ignoring our own clipboard ownership announcement");
        offer.destroy();
        return Ok(());
    }

    // Gerenciadores de senha (KeePassXC, Bitwarden, Klipper) anexam este MIME
    // para pedir que o conteúdo não seja registrado no histórico. Também não
    // tomamos ownership: o segredo deve expirar quando o gerenciador limpar.
    if mimes.iter().any(|m| m == PASSWORD_MANAGER_HINT_MIME) {
        debug!("Password manager hint present; not recording clipboard item");
        offer.destroy();
        return Ok(());
    }

    let mime = match WaylandState::select_mime(&mimes) {
        Some(m) => m.to_string(),
        None => {
            debug!("No supported MIME type in offer (available: {:?})", mimes);
            offer.destroy();
            return Ok(());
        }
    };

    // Cria pipe: compositor escreve no write_end, lemos do read_end
    let (read_end, write_end) = os_pipe::pipe()
        .map_err(|e| anyhow!("Failed to create pipe: {}", e))?;

    // Solicita ao compositor que envie o conteúdo pelo pipe
    offer.receive(mime.clone(), write_end.as_fd());

    // Flush envia o request para o compositor processar
    conn.flush().map_err(|e| anyhow!("Wayland flush failed: {}", e))?;

    // Destrói a offer (já fizemos o receive)
    offer.destroy();

    // Drop do write_end: necessário para que read_to_end() termine
    // (quando o compositor fechar o fd, EOF chega no read_end)
    drop(write_end);

    // Lê o conteúdo do pipe
    // Nota: leitura bloqueante é OK aqui pois estamos em thread dedicada
    let data = read_pipe(read_end)?;

    if data.is_empty() {
        return Ok(());
    }

    let hash = hash_bytes(&data);
    if hash == state.last_hash {
        debug!("Clipboard unchanged (dedup by hash)");
        return Ok(());
    }

    state.last_hash = hash.clone();

    debug!(
        mime = %mime,
        bytes = data.len(),
        hash = &hash[..8],
        "Wayland clipboard captured"
    );

    // Emite evento para o loop principal do daemon
    let _ = state.event_tx.send(ClipboardEvent {
        hash,
        data,
        mime_type: mime,
    });

    Ok(())
}

/// Tempo máximo esperando o dono do clipboard escrever no pipe.
/// Sem isso, um app travado congela o monitor para sempre.
const READ_TIMEOUT: Duration = Duration::from_secs(15);
/// Limite duro de tamanho — protege contra fontes que despejam dados sem fim.
const MAX_CLIPBOARD_BYTES: usize = 64 * 1024 * 1024;

fn read_pipe(mut reader: PipeReader) -> Result<Vec<u8>> {
    use std::os::fd::AsRawFd;

    let fd = reader.as_raw_fd();

    // Modo não-bloqueante + poll com deadline: a leitura nunca trava o monitor
    // SAFETY: fcntl em fd válido que possuímos
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        if flags < 0 || libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
            return Err(anyhow!(
                "fcntl(O_NONBLOCK) failed: {}",
                std::io::Error::last_os_error()
            ));
        }
    }

    let deadline = Instant::now() + READ_TIMEOUT;
    let mut buf = Vec::with_capacity(4096);
    let mut chunk = [0u8; 64 * 1024];

    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(anyhow!(
                "timed out after {:?} waiting for clipboard owner to write",
                READ_TIMEOUT
            ));
        }

        let mut pfd = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
        // SAFETY: pollfd válido apontando para fd que possuímos
        let n = unsafe { libc::poll(&mut pfd, 1, remaining.as_millis() as libc::c_int) };
        if n < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(anyhow!("poll on clipboard pipe failed: {}", err));
        }
        if n == 0 {
            continue; // poll expirou — o check do deadline acima encerra
        }

        match reader.read(&mut chunk) {
            Ok(0) => break, // EOF — dono fechou o pipe
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if buf.len() > MAX_CLIPBOARD_BYTES {
                    return Err(anyhow!(
                        "clipboard content exceeds {} bytes, aborting read",
                        MAX_CLIPBOARD_BYTES
                    ));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(anyhow!("Failed to read clipboard pipe: {}", e)),
        }
    }

    Ok(buf)
}

fn hash_bytes(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    hex::encode(h.finalize())
}

// ─── Ownership (persistência do clipboard) ────────────────────────────────────

/// MIME marcador anunciado junto com nossas cópias — permite ao monitor
/// reconhecer (e ignorar) anúncios de ownership do próprio Clypse.
pub const CLYPSE_MARKER_MIME: &str = "application/x-clypse";

/// Convenção do KDE adotada pelos gerenciadores de senha: a presença deste
/// MIME numa offer sinaliza conteúdo sensível que não deve ir ao histórico.
const PASSWORD_MANAGER_HINT_MIME: &str = "x-kde-passwordManagerHint";

/// Toma ownership do clipboard via wl-clipboard-rs.
/// Garante que o conteúdo persiste quando o app original fecha.
/// Roda em thread separada para não bloquear o daemon.
pub fn take_ownership(data: Vec<u8>, mime_type: &str) {
    use wl_clipboard_rs::copy::{copy_multi, MimeSource, MimeType as CopyMime, Options, Source};

    if !(mime_type.starts_with("text/") || mime_type.starts_with("image/")) {
        return;
    }

    let sources = vec![
        MimeSource {
            source:    Source::Bytes(data.into_boxed_slice()),
            mime_type: CopyMime::Specific(mime_type.to_string()),
        },
        MimeSource {
            source:    Source::Bytes(vec![b'1'].into_boxed_slice()),
            mime_type: CopyMime::Specific(CLYPSE_MARKER_MIME.to_string()),
        },
    ];

    std::thread::Builder::new()
        .name("wl-clip-owner".into())
        .spawn(move || {
            if let Err(e) = copy_multi(Options::new(), sources) {
                debug!("Clipboard ownership ended: {}", e);
            }
        })
        .ok();
}
