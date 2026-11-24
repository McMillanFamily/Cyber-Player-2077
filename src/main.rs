use std::{cell::RefCell, fs::File, io::BufReader, path::Path, time::Duration};

use i_slint_backend_winit::WinitWindowAccessor;
use rodio::{Decoder, OutputStream, OutputStreamHandle, Sink};
use slint::ComponentHandle;

slint::include_modules!();


// rodio's OutputStream is !Send, so the player lives in thread-local storage
// on the UI thread. All access goes through the Slint event loop.
struct Player {
    _stream: OutputStream, // must stay alive or audio stops
    handle: OutputStreamHandle,
    sink: Option<Sink>,
}

thread_local! {
    static PLAYER: RefCell<Option<Player>> = const { RefCell::new(None) };
}

fn init_player() -> Result<(), String> {
    let (stream, handle) = OutputStream::try_default().map_err(|e| e.to_string())?;
    PLAYER.with(|p| {
        *p.borrow_mut() = Some(Player {
            _stream: stream,
            handle,
            sink: None,
        });
    });
    Ok(())
}

fn load_track(path: &Path) -> Result<(), String> {
    let file = File::open(path).map_err(|e| e.to_string())?;
    let decoder = Decoder::new(BufReader::new(file)).map_err(|e| e.to_string())?;

    PLAYER.with(|p| {
        let mut p = p.borrow_mut();
        let player = p.as_mut().ok_or_else(|| "NO AUDIO DEVICE".to_string())?;

        if let Some(old) = player.sink.take() {
            old.stop();
        }
        let sink = Sink::try_new(&player.handle).map_err(|e| e.to_string())?;
        sink.append(decoder);
        player.sink = Some(sink);
        Ok::<(), String>(())
    })
}

fn main() -> Result<(), slint::PlatformError> {
    let ui = AppWindow::new()?;

    if let Err(e) = init_player() {
        ui.set_track_name(format!("AUDIO_ERR // {e}").into());
    }

    // --- Load file -------------------------------------------------------
    // The file dialog blocks, so run it on a worker thread and hop back to
    // the UI thread with the chosen path.
    ui.on_load_file({
        let weak = ui.as_weak();
        move || {
            let weak = weak.clone();
            std::thread::spawn(move || {
                let picked = rfd::FileDialog::new()
                    .add_filter("Audio", &["mp3", "wav", "flac", "ogg"])
                    .pick_file();

                let Some(path) = picked else { return };

                let _ = slint::invoke_from_event_loop(move || {
                    let Some(ui) = weak.upgrade() else { return };
                    match load_track(&path) {
                        Ok(()) => {
                            let name = path
                                .file_name()
                                .map(|n| n.to_string_lossy().to_uppercase())
                                .unwrap_or_default();
                            ui.set_track_name(format!("TRACK // {name}").into());
                            ui.set_is_playing(true); // sink starts playing immediately
                        }
                        Err(e) => {
                            ui.set_track_name(format!("LOAD_ERR // {e}").into());
                            ui.set_is_playing(false);
                        }
                    }
                });
            });
        }
    });

    // --- Play / pause ----------------------------------------------------
    ui.on_toggle_play({
        let weak = ui.as_weak();
        move || {
            let Some(ui) = weak.upgrade() else { return };

            let now_playing = PLAYER.with(|p| {
                let p = p.borrow();
                let sink = p.as_ref()?.sink.as_ref()?;
                if sink.empty() {
                    Some(false) // track finished
                } else if sink.is_paused() {
                    sink.play();
                    Some(true)
                } else {
                    sink.pause();
                    Some(false)
                }
            });

            if let Some(v) = now_playing {
                ui.set_is_playing(v);
            }
        }
    });

    // --- Close -----------------------------------------------------------
    ui.on_close_window({
        let weak = ui.as_weak();
        move || {
            if let Some(ui) = weak.upgrade() {
                let _ = ui.hide();
            }
        }
    });

    // --- Drag (frameless window) ----------------------------------------
    ui.on_start_drag({
        let weak = ui.as_weak();
        move || {
            if let Some(ui) = weak.upgrade() {
                ui.window().with_winit_window(|w| {
                    let _ = w.drag_window();
                });
            }
        }
    });

    // --- Reset the UI state when a track finishes on its own -------------
    let end_timer = slint::Timer::default();
    end_timer.start(slint::TimerMode::Repeated, Duration::from_millis(250), {
        let weak = ui.as_weak();
        move || {
            let Some(ui) = weak.upgrade() else { return };
            if !ui.get_is_playing() {
                return;
            }
            let finished = PLAYER.with(|p| {
                p.borrow()
                    .as_ref()
                    .and_then(|pl| pl.sink.as_ref())
                    .map(|s| s.empty())
                    .unwrap_or(false)
            });
            if finished {
                ui.set_is_playing(false);
            }
        }
    });

    ui.run()
}
