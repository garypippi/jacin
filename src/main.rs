use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use calloop::{
    EventLoop, LoopHandle, LoopSignal, RegistrationToken,
    ping::make_ping,
    signals::{Signal, Signals},
    timer::{TimeoutAction, Timer},
};
use calloop_wayland_source::WaylandSource;
use wayland_client::{
    Connection,
    globals::registry_queue_init,
    protocol::{wl_compositor, wl_shm},
};
use wayland_protocols_misc::zwp_input_method_v2::client::zwp_input_method_manager_v2;
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::zwp_virtual_keyboard_manager_v1;

mod config;
mod coordinator;
mod dispatch;
mod input;
mod keysym;
mod model;
mod neovim;
mod state;
mod ui;

use model::Model;
use neovim::Neovim;
use state::{Animations, KeyRepeatState, KeyboardState, WaylandState};
use ui::{TextRenderer, UnifiedPopup};

fn main() -> anyhow::Result<()> {
    env_logger::init();

    let mut config = config::Config::load();
    if std::env::args().any(|a| a == "--clean") {
        config.clean = true;
    }

    let conn = Connection::connect_to_env()?;
    log::info!("Connected to Wayland display");

    let (globals, event_queue) = registry_queue_init::<State>(&conn)?;
    let qh = event_queue.handle();

    let input_method_manager: zwp_input_method_manager_v2::ZwpInputMethodManagerV2 = globals
        .bind(&qh, 1..=1, ())
        .expect("zwp_input_method_manager_v2 not available - is this a wlroots compositor?");
    log::info!("Bound zwp_input_method_manager_v2");

    // Get the seat (assuming single seat)
    let seat: wayland_client::protocol::wl_seat::WlSeat =
        globals.bind(&qh, 1..=9, ()).expect("wl_seat not available");

    let compositor: wl_compositor::WlCompositor = globals
        .bind(&qh, 4..=6, ())
        .expect("wl_compositor not available");

    let shm: wl_shm::WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm not available");

    let input_method = input_method_manager.get_input_method(&seat, &qh, ());
    log::info!("Created zwp_input_method_v2");

    // Create virtual keyboard for clearing stuck modifier state
    let virtual_keyboard = match globals
        .bind::<zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1, _, _>(&qh, 1..=1, ())
    {
        Ok(manager) => {
            let vk = manager.create_virtual_keyboard(&seat, &qh, ());
            log::info!("Created zwp_virtual_keyboard_v1");
            Some(vk)
        }
        Err(e) => {
            log::warn!(
                "zwp_virtual_keyboard_manager_v1 not available: {} (modifier clearing disabled)",
                e
            );
            None
        }
    };

    let font_size = config
        .font
        .size
        .filter(|s| s.is_finite() && *s > 0.0)
        .map(|s| s.clamp(8.0, 48.0))
        .unwrap_or(16.0);
    let text_renderer = TextRenderer::new_with_family(font_size, config.font.family.as_deref());
    let mono_renderer =
        TextRenderer::new_monospace_with_family(font_size, config.font.mono_family.as_deref());
    if text_renderer.is_none() {
        log::warn!("Font not available, popup window disabled");
    }

    // The popup surface is automatically positioned near the cursor by the compositor
    let mut popup = if let (Some(renderer), Some(mono)) = (text_renderer, mono_renderer) {
        match UnifiedPopup::new(&compositor, &input_method, &shm, &qh, renderer, mono) {
            Some(win) => {
                log::info!("Unified popup window created (using input popup surface)");
                Some(win)
            }
            None => {
                log::warn!("Failed to create unified popup window");
                None
            }
        }
    } else {
        None
    };

    // Size Neovim's UI to what the popup can show, so long lines wrap at
    // the popup width and Neovim scrolls the window itself
    let ui_grid_size = popup.as_mut().map(|popup| popup.grid_ui_size());
    if let Some((cols, rows)) = ui_grid_size {
        log::info!("[IME] UI grid size: {}x{}", cols, rows);
    }

    let mut event_loop: EventLoop<'static, State> = EventLoop::try_new()?;

    let mut state = State {
        loop_handle: event_loop.handle(),
        loop_signal: None,
        wayland: {
            let mut ws = WaylandState::new(qh.clone(), input_method);
            ws.virtual_keyboard = virtual_keyboard;
            ws
        },
        keyboard: KeyboardState::new(),
        repeat: KeyRepeatState::new(),
        model: Model::new(),
        animations: Animations::new(),
        pending_exit: false,
        toggle_flag: Arc::new(AtomicBool::new(false)),
        config: config.clone(),
        nvim: None,
        nvim_write_token: None,
        ui_grid_size,
        popup,
        repeat_timer_token: None,
        animation_timer_token: None,
        popup_dirty: false,
    };

    state.loop_signal = Some(event_loop.get_signal());

    if !state.start_nvim() {
        log::warn!("Continuing without Neovim backend");
    }

    WaylandSource::new(conn, event_queue).insert(event_loop.handle())?;

    let loop_signal = state.loop_signal.clone();
    let exit_signals = Signals::new(&[Signal::SIGINT, Signal::SIGTERM])?;
    event_loop
        .handle()
        .insert_source(exit_signals, move |_, _, _| {
            log::info!("Received signal, exiting...");
            if let Some(ref signal) = loop_signal {
                signal.stop();
            }
        })?;

    // Set up SIGUSR1 for IME toggle (triggered by: pkill -SIGUSR1 jacin)
    // Use a ping to wake up the event loop when signal arrives
    let (ping, ping_source) = make_ping()?;
    let toggle_flag_clone = state.toggle_flag.clone();

    // Register signal handler that sets flag AND pings the event loop
    let ping_clone = ping.clone();
    unsafe {
        signal_hook::low_level::register(signal_hook::consts::SIGUSR1, move || {
            toggle_flag_clone.store(true, Ordering::SeqCst);
            ping_clone.ping();
        })?;
    }

    // Add ping source to event loop (just to wake it up, we handle toggle in the callback)
    event_loop
        .handle()
        .insert_source(ping_source, |_, _, _| {})?;

    // Small delay to let any pending key events (like Enter from "cargo run") clear
    std::thread::sleep(std::time::Duration::from_millis(500));

    log::info!("Entering event loop... (Ctrl+C to exit)");
    log::info!("Focus a text input field to activate the IME");

    let handle = event_loop.handle();
    event_loop.run(None, &mut state, |state| {
        if state.toggle_flag.swap(false, Ordering::SeqCst) {
            state.handle_ime_toggle();
        }

        // Insert on-demand repeat timer when a key is held
        if state.repeat.has_key() && state.repeat_timer_token.is_none() {
            match handle.insert_source(
                Timer::from_duration(std::time::Duration::from_millis(5)),
                |_, _, state| {
                    if state.model.ime.is_fully_enabled()
                        && let Some(key) = state
                            .repeat
                            .should_fire(state.keyboard.repeat_rate, state.keyboard.repeat_delay)
                    {
                        state.handle_repeat_key(key);
                    }
                    if state.repeat.has_key() {
                        TimeoutAction::ToDuration(std::time::Duration::from_millis(5))
                    } else {
                        state.repeat_timer_token = None;
                        TimeoutAction::Drop
                    }
                },
            ) {
                Ok(token) => state.repeat_timer_token = Some(token),
                Err(e) => {
                    log::error!("[TIMER] Failed to insert repeat timer: {e}");
                    state.repeat_timer_token = None;
                    state.repeat.cancel();
                }
            }
        }

        // On-demand timer for the REC blink and transient message expiry
        let needs_blink =
            state.config.behavior.recording_blink && !state.model.view.recording.is_empty();
        let needs_timer = needs_blink || state.model.view.has_transient_message();
        if needs_timer && state.animation_timer_token.is_none() {
            match handle.insert_source(
                Timer::from_duration(std::time::Duration::from_millis(100)),
                |_, _, state| {
                    let now = std::time::Instant::now();
                    let mut changed = false;

                    // Advance all animations (currently: REC blink)
                    if state.config.behavior.recording_blink {
                        changed |= state
                            .animations
                            .update_all(now, &state.model.view.recording);
                    }

                    // Expire transient message
                    changed |= state.model.view.expire_transient_message();

                    let needs_blink = state.config.behavior.recording_blink
                        && !state.model.view.recording.is_empty();
                    let keep_running = needs_blink || state.model.view.has_transient_message();
                    if !keep_running {
                        state.update_popup();
                        state.animation_timer_token = None;
                        TimeoutAction::Drop
                    } else {
                        if changed {
                            state.update_popup();
                        }
                        TimeoutAction::ToDuration(std::time::Duration::from_millis(100))
                    }
                },
            ) {
                Ok(token) => state.animation_timer_token = Some(token),
                Err(e) => {
                    log::error!("[TIMER] Failed to insert animation timer: {e}");
                    state.animation_timer_token = None;
                }
            }
        }

        // Render the popup once per iteration for all changes above
        // (key handling, Neovim messages, timers)
        state.flush_popup();

        // Requests the stdin pipe could not take yet
        state.watch_nvim_writable();

        if state.pending_exit
            && let Some(ref signal) = state.loop_signal
        {
            signal.stop();
        }
    })?;

    state.wayland.release_keyboard();
    if let Some(nvim) = state.nvim.take() {
        nvim.shutdown();
    }
    if let Some(window) = state.popup.take() {
        window.destroy();
    }

    log::info!("Goodbye!");

    // Force clean exit to avoid any stuck keyboard state
    std::process::exit(0);
}

pub struct State {
    // For event sources added later (Neovim pipes, timers)
    pub(crate) loop_handle: LoopHandle<'static, State>,
    pub(crate) loop_signal: Option<LoopSignal>,
    pub(crate) wayland: WaylandState,
    pub(crate) keyboard: KeyboardState,
    pub(crate) repeat: KeyRepeatState,
    // IME model driven by Neovim messages (NvimView, sent keys)
    pub(crate) model: Model,
    pub(crate) animations: Animations,
    pub(crate) pending_exit: bool,
    pub(crate) toggle_flag: Arc<AtomicBool>,
    // Config (needed for Neovim respawn after :q)
    pub(crate) config: config::Config,
    pub(crate) nvim: Option<Neovim>,
    // Source flushing requests to Neovim's stdin (only while the pipe is full)
    pub(crate) nvim_write_token: Option<RegistrationToken>,
    // Neovim UI size fitted to the popup (re-applied on respawn)
    pub(crate) ui_grid_size: Option<(usize, usize)>,
    // Unified popup window (Neovim window grid, mode, candidates)
    pub(crate) popup: Option<UnifiedPopup>,
    // On-demand timer tokens (None = timer not running)
    pub(crate) repeat_timer_token: Option<RegistrationToken>,
    pub(crate) animation_timer_token: Option<RegistrationToken>,
    // Popup redraw requested; rendered once at the end of the loop iteration
    pub(crate) popup_dirty: bool,
}
