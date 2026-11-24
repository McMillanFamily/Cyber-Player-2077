use std::{cell::RefCell, fs::File, io::BufReader, path::Path, time::Duration};

use i_slint_backend_winit::WinitWindowAccessor;
use rodio::{Decoder, OutputStream, OutputStreamHandle, Sink, Source};
use slint::ComponentHandle;

slint::include_modules!();

struct Player {
    _stream: OutputStream, // must stay alive or audio stops
    handle: OutputStreamHandle,
    sink: Option<Sink>,
    total: Option<Duration>, // None if the decoder can't tell us (common for some MP3s)
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
            total: None,
        });
    });
    Ok(())
}

fn load_track(path: &Path) -> Result<(), String> {
    let file = File::open(path).map_err(|e| e.to_string())?;
    let decoder = Decoder::new(BufReader::new(file)).map_err(|e| e.to_string())?;
    let total = decoder.total_duration();

    PLAYER.with(|p| {
        let mut p = p.borrow_mut();
        let player = p.as_mut().ok_or_else(|| "NO AUDIO DEVICE".to_string())?;

        if let Some(old) = player.sink.take() {
            old.stop();
        }
        let sink = Sink::try_new(&player.handle).map_err(|e| e.to_string())?;
        sink.append(decoder);
        player.sink = Some(sink);
        player.total = total;
        Ok::<(), String>(())
    })
}

fn fmt_time(d: Duration) -> String {
    let secs = d.as_secs();
    format!("{}:{:02}", secs / 60, secs % 60)
}

fn main() -> Result<(), slint::PlatformError> {
    let ui = AppWindow::new()?;

    if let Err(e) = init_player() {
        ui.set_track_name(format!("AUDIO_ERR // {e}").into());
    }

    // --- Load file -------------------------------------------------------
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
                            ui.set_progress(0.0);
                            ui.set_is_playing(true);
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

    // --- Seek (click on progress bar) -------------------------------------
    ui.on_seek(|fraction: f32| {
        PLAYER.with(|p| {
            let p = p.borrow();
            if let Some(player) = p.as_ref() {
                if let (Some(sink), Some(total)) = (player.sink.as_ref(), player.total) {
                    let target = total.mul_f32(fraction.clamp(0.0, 1.0));
                    let _ = sink.try_seek(target);
                }
            }
        });
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

    // --- Progress / end-of-track polling ---------------------------------
    let progress_timer = slint::Timer::default();
    progress_timer.start(slint::TimerMode::Repeated, Duration::from_millis(100), {
        let weak = ui.as_weak();
        move || {
            let Some(ui) = weak.upgrade() else { return };

            let snapshot = PLAYER.with(|p| {
                let p = p.borrow();
                let player = p.as_ref()?;
                let sink = player.sink.as_ref()?;
                Some((sink.get_pos(), player.total, sink.empty()))
            });
            let Some((pos, total, finished)) = snapshot else { return };

            ui.set_elapsed_text(fmt_time(pos).into());

            match total {
                Some(t) if !t.is_zero() => {
                    ui.set_total_text(fmt_time(t).into());
                    ui.set_progress((pos.as_secs_f32() / t.as_secs_f32()).min(1.0));
                }
                _ => {
                    ui.set_total_text("--:--".into());
                    ui.set_progress(0.0);
                }
            }

            if finished && ui.get_is_playing() {
                ui.set_is_playing(false);
            }
        }
    });

    ui.run()
}