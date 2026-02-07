use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use calloop::{
    EventLoop, LoopSignal,
    ping::make_ping,
    signals::{Signal, Signals},
    timer::{TimeoutAction, Timer},
};
use calloop_wayland_source::WaylandSource;
use wayland_client::{
    Connection,
    globals::registry_queue_init,
    protocol::{wl_compositor, wl_keyboard, wl_shm},
};
use wayland_protocols_misc::zwp_input_method_v2::client::zwp_input_method_manager_v2;
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::zwp_virtual_keyboard_manager_v1;

mod config;
mod coordinator;
mod dispatch;
mod input;
mod neovim;
mod state;
mod ui;

use neovim::{NeovimHandle, VisualSelection};
use state::{ImeState, KeyRepeatState, KeyboardState, KeypressState, WaylandState};
use ui::{TextRenderer, UnifiedPopup};

fn main() -> anyhow::Result<()> {
    // Load configuration
    let config = config::Config::load();

    // Connect to Wayland display
    let conn = Connection::connect_to_env()?;
    eprintln!("Connected to Wayland display");

    // Initialize registry and get globals
    let (globals, event_queue) = registry_queue_init::<State>(&conn)?;
    let qh = event_queue.handle();

    // Bind input method manager
    let input_method_manager: zwp_input_method_manager_v2::ZwpInputMethodManagerV2 = globals
        .bind(&qh, 1..=1, ())
        .expect("zwp_input_method_manager_v2 not available - is this a wlroots compositor?");
    eprintln!("Bound zwp_input_method_manager_v2");

    // Get the seat (assuming single seat)
    let seat: wayland_client::protocol::wl_seat::WlSeat =
        globals.bind(&qh, 1..=9, ()).expect("wl_seat not available");

    // Bind compositor and shm for candidate window
    let compositor: wl_compositor::WlCompositor = globals
        .bind(&qh, 4..=6, ())
        .expect("wl_compositor not available");

    let shm: wl_shm::WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm not available");

    // Create input method for this seat
    let input_method = input_method_manager.get_input_method(&seat, &qh, ());
    eprintln!("Created zwp_input_method_v2");

    // Create virtual keyboard for clearing stuck modifier state
    let virtual_keyboard = match globals
        .bind::<zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1, _, _>(&qh, 1..=1, ())
    {
        Ok(manager) => {
            let vk = manager.create_virtual_keyboard(&seat, &qh, ());
            eprintln!("Created zwp_virtual_keyboard_v1");
            Some(vk)
        }
        Err(e) => {
            eprintln!("zwp_virtual_keyboard_manager_v1 not available: {} (modifier clearing disabled)", e);
            None
        }
    };

    // Spawn Neovim backend
    let nvim = match neovim::spawn_neovim(config.clone()) {
        Ok(handle) => {
            eprintln!("Neovim backend spawned");
            Some(handle)
        }
        Err(e) => {
            eprintln!("Failed to spawn Neovim: {} (continuing without backend)", e);
            None
        }
    };

    // Try to create text renderer for unified popup window
    let text_renderer = TextRenderer::new(16.0);
    if text_renderer.is_none() {
        eprintln!("Warning: Font not available, popup window disabled");
    }

    // Create unified popup window using input method popup surface
    // The popup surface is automatically positioned near the cursor by the compositor
    let popup = if let Some(renderer) = text_renderer {
        match UnifiedPopup::new(&compositor, &input_method, &shm, &qh, renderer) {
            Some(win) => {
                eprintln!("Unified popup window created (using input popup surface)");
                Some(win)
            }
            None => {
                eprintln!("Failed to create unified popup window");
                None
            }
        }
    } else {
        None
    };

    // Create application state
    let mut state = State {
        loop_signal: None,
        wayland: {
            let mut ws = WaylandState::new(qh.clone(), input_method);
            ws.virtual_keyboard = virtual_keyboard;
            ws
        },
        keyboard: KeyboardState::new(),
        repeat: KeyRepeatState::new(),
        ime: ImeState::new(),
        keypress: KeypressState::new(),
        pending_exit: false,
        toggle_flag: Arc::new(AtomicBool::new(false)),
        reactivation_count: 0,
        nvim,
        visual_display: None,
        popup,
        config,
    };

    // Set up calloop event loop
    let mut event_loop: EventLoop<State> = EventLoop::try_new()?;
    state.loop_signal = Some(event_loop.get_signal());

    // Insert Wayland event source
    WaylandSource::new(conn, event_queue).insert(event_loop.handle())?;

    // Set up signal handling for clean exit
    let loop_signal = state.loop_signal.clone();
    let exit_signals = Signals::new(&[Signal::SIGINT, Signal::SIGTERM])?;
    event_loop
        .handle()
        .insert_source(exit_signals, move |_, _, _| {
            eprintln!("\nReceived signal, exiting...");
            if let Some(ref signal) = loop_signal {
                signal.stop();
            }
        })?;

    // Set up SIGUSR1 for IME toggle (triggered by: pkill -SIGUSR1 custom-ime)
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

    // Add timer for keypress display timeout (fires every 100ms to check)
    let timer = Timer::from_duration(std::time::Duration::from_millis(100));
    event_loop
        .handle()
        .insert_source(timer, |_, _, state| {
            // Check for keypress display timeout
            if state.keypress.should_show() && state.keypress.is_timed_out() {
                state.hide_keypress();
            }
            // Re-arm the timer to fire again
            TimeoutAction::ToDuration(std::time::Duration::from_millis(100))
        })
        .expect("Failed to insert timer source");

    // Add timer for key repeat (~5ms polling interval)
    let repeat_timer = Timer::from_duration(std::time::Duration::from_millis(5));
    event_loop
        .handle()
        .insert_source(repeat_timer, |_, _, state| {
            if state.ime.is_fully_enabled()
                && let Some(key) = state.repeat.should_fire(
                    state.keyboard.repeat_rate,
                    state.keyboard.repeat_delay,
                )
            {
                state.handle_key(
                    key,
                    wl_keyboard::KeyState::Pressed,
                    input::KeyOrigin::Repeat,
                );
            }
            TimeoutAction::ToDuration(std::time::Duration::from_millis(5))
        })
        .expect("Failed to insert repeat timer source");

    // Small delay to let any pending key events (like Enter from "cargo run") clear
    std::thread::sleep(std::time::Duration::from_millis(500));

    eprintln!("Entering event loop... (Ctrl+C to exit)");
    eprintln!("Focus a text input field to activate the IME");

    // Run the event loop
    event_loop.run(None, &mut state, |state| {
        // Check for IME toggle signal (SIGUSR1)
        if state.toggle_flag.swap(false, Ordering::SeqCst) {
            state.handle_ime_toggle();
        }

        // Check for messages from Neovim
        // Collect messages first to avoid borrow conflict
        let messages: Vec<_> = state
            .nvim
            .as_ref()
            .map(|nvim| std::iter::from_fn(|| nvim.try_recv()).collect())
            .unwrap_or_default();

        for msg in messages {
            state.handle_nvim_message(msg);
        }

        if state.pending_exit
            && let Some(ref signal) = state.loop_signal
        {
            signal.stop();
        }
    })?;

    // Cleanup
    state.wayland.release_keyboard();
    if let Some(ref nvim) = state.nvim {
        nvim.shutdown();
    }
    if let Some(window) = state.popup.take() {
        window.destroy();
    }

    eprintln!("Goodbye!");

    // Force clean exit to avoid any stuck keyboard state
    std::process::exit(0);
}

pub struct State {
    pub(crate) loop_signal: Option<LoopSignal>,
    // Component state structs
    pub(crate) wayland: WaylandState,
    pub(crate) keyboard: KeyboardState,
    pub(crate) repeat: KeyRepeatState,
    pub(crate) ime: ImeState,
    pub(crate) keypress: KeypressState,
    // Exit and toggle flags
    pub(crate) pending_exit: bool,
    pub(crate) toggle_flag: Arc<AtomicBool>,
    // Counter for consecutive Deactivate/Activate re-grabs without user key input.
    // Prevents infinite loop when compositor keeps cycling.
    pub(crate) reactivation_count: u32,
    // Neovim backend
    pub(crate) nvim: Option<NeovimHandle>,
    // Transient visual selection display state (observed from Neovim, not IME-owned)
    pub(crate) visual_display: Option<VisualSelection>,
    // Unified popup window (preedit, keypress, candidates)
    pub(crate) popup: Option<UnifiedPopup>,
    // Configuration
    pub(crate) config: config::Config,
}
