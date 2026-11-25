#![allow(non_snake_case)] // camelCase on purpose, so rustc doesn't nag about it rust is annoying like that :D

use std::{
    cell::RefCell,
    fs::File,
    io::BufReader,
    path::{Path, PathBuf},
    time::Duration,
};

use i_slint_backend_winit::WinitWindowAccessor;
use rodio::{Decoder, OutputStream, OutputStreamHandle, Sink, Source};
use slint::ComponentHandle;

slint::include_modules!();

const EXTS: &[&str] = &["mp3", "wav", "flac", "ogg"];

struct Player {
    _stream: OutputStream, // if this gets dropped the audio just dies, so it lives here
    handle: OutputStreamHandle,
    sink: Option<Sink>,
    total: Option<Duration>, // some mp3s don't report a length, so this can be None
    list: Vec<PathBuf>,
    cur: usize, // index of the track that's playing right now
    vol: f32,   // slider position, 0.0 to 1.0
}

thread_local! {
    static PLAYER: RefCell<Option<Player>> = const { RefCell::new(None) };
}

// Slider -> real volume. Squaring it feels way more natural than a straight 
fn amp(v: f32) -> f32 {
    let v = v.clamp(0.0, 1.0);
    v * v
}

fn initPlayer() -> Result<(), String> {
    let (stream, handle) = OutputStream::try_default().map_err(|e| e.to_string())?;
    PLAYER.with(|p| {
        *p.borrow_mut() = Some(Player {
            _stream: stream,
            handle,
            sink: None,
            total: None,
            list: Vec::new(),
            cur: 0,
            vol: 0.8,
        });
    });
    Ok(())
}

fn loadTrack(path: &Path) -> Result<(), String> {
    let file = File::open(path).map_err(|e| e.to_string())?;
    let dec = Decoder::new(BufReader::new(file)).map_err(|e| e.to_string())?;
    let total = dec.total_duration();

    PLAYER.with(|p| {
        let mut p = p.borrow_mut();
        let pl = p.as_mut().ok_or_else(|| "NO AUDIO DEVICE".to_string())?;

        // kill the old track before starting the new one 
        if let Some(old) = pl.sink.take() {
            old.stop();
        }
        let sink = Sink::try_new(&pl.handle).map_err(|e| e.to_string())?;
        sink.set_volume(amp(pl.vol)); // new sinks start at 100%, so reapply our volume
        sink.append(dec);
        pl.sink = Some(sink);
        pl.total = total;
        Ok::<(), String>(())
    })
}

// Grab every audio file in the folder (not subfolders), sorted by name so
// numbered albums play in the right order.
fn scanDir(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.extension()
                    .and_then(|e| e.to_str())
                    .map(|e| EXTS.contains(&e.to_ascii_lowercase().as_str())) 
                    .unwrap_or(false)
        })
        .collect();

    files.sort_by_key(|p| {
        p.file_name()
            .map(|n| n.to_string_lossy().to_lowercase())
            .unwrap_or_default()
    });
    files //This took 3 hours kill meeee
}

// Swap in a new playlist and start from the top
fn setList(ui: &AppWindow, list: Vec<PathBuf>) {
    PLAYER.with(|p| {
        if let Some(pl) = p.borrow_mut().as_mut() {
            pl.list = list;
            pl.cur = 0;
        }
    });
    startTrack(ui, 0);
}

// Play list[i]. Returns false if there's no such track or it won't load.
fn startTrack(ui: &AppWindow, i: usize) -> bool {
    let (path, len) = PLAYER.with(|p| {
        let p = p.borrow();
        match p.as_ref() {
            Some(pl) => (pl.list.get(i).cloned(), pl.list.len()),
            None => (None, 0),
        }
    });
    let Some(path) = path else { return false };

    match loadTrack(&path) {
        Ok(()) => {
            PLAYER.with(|p| {
                if let Some(pl) = p.borrow_mut().as_mut() {
                    pl.cur = i;
                }
            });
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_uppercase())
                .unwrap_or_default();
            ui.set_trackName(format!("[{}/{}] {name}", i + 1, len).into());
            ui.set_progress(0.0);
            ui.set_elapsedText("0:00".into());
            ui.set_isPlaying(true);
            true
        }
        Err(e) => {
            ui.set_trackName(format!("LOAD_ERR // {e}").into());
            ui.set_isPlaying(false);
            false
        } //This caused a 4 hour fight with rust i fucking hate this 
    }
}

// Jump forward or back by d tracks. Returns false if we'd fall off either end.
fn step(ui: &AppWindow, d: isize) -> bool {
    let (cur, len) = PLAYER.with(|p| {
        p.borrow()
            .as_ref()
            .map(|pl| (pl.cur, pl.list.len()))
            .unwrap_or((0, 0))
    });
    let next = cur as isize + d;
    if next < 0 || next >= len as isize {
        return false;
    }
    startTrack(ui, next as usize)
}

fn fmtTime(d: Duration) -> String {
    let s = d.as_secs();
    format!("{}:{:02}", s / 60, s % 60)
}

fn main() -> Result<(), slint::PlatformError> {
    let ui = AppWindow::new()?;

    if let Err(e) = initPlayer() {
        ui.set_trackName(format!("AUDIO_ERR // {e}").into());
    }

    // --- Open a single file ---------------------------------------------
    ui.on_loadFile({
        let weak = ui.as_weak();
        move || {
            let weak = weak.clone();
            // the file dialog blocks, so it gets its own thread
            std::thread::spawn(move || {
                let Some(path) = rfd::FileDialog::new()
                    .add_filter("Audio", EXTS)
                    .pick_file()
                else {
                    return;
                };

                let _ = slint::invoke_from_event_loop(move || {
                    let Some(ui) = weak.upgrade() else { return };
                    setList(&ui, vec![path]);
                });
            });
        }
    });

    // --- Open a whole folder --------------------------------------------
    ui.on_loadDir({
        let weak = ui.as_weak();
        move || {
            let weak = weak.clone();
            std::thread::spawn(move || {
                let Some(dir) = rfd::FileDialog::new().pick_folder() else {
                    return;
                };
                // scan here, not on the UI thread, so big folders don't freeze the window
                let tracks = scanDir(&dir);

                let _ = slint::invoke_from_event_loop(move || {
                    let Some(ui) = weak.upgrade() else { return };
                    if tracks.is_empty() {
                        ui.set_trackName("NO_AUDIO // FOLDER EMPTY".into());
                    } else {
                        setList(&ui, tracks);
                    }
                });
            });
        }
    });

    // --- Next / previous ------------------------------------------------
    ui.on_next({
        let weak = ui.as_weak();
        move || {
            if let Some(ui) = weak.upgrade() {
                step(&ui, 1);
            }
        }
    });

    ui.on_prev({
        let weak = ui.as_weak();
        move || {
            if let Some(ui) = weak.upgrade() {
                // nothing before the first track, so just restart it
                if !step(&ui, -1) {
                    startTrack(&ui, 0);
                }
            }
        }
    });

    // --- Volume ---------------------------------------------------------
    ui.on_setVolume(|v| {
        PLAYER.with(|p| {
            if let Some(pl) = p.borrow_mut().as_mut() {
                pl.vol = v.clamp(0.0, 1.0);
                if let Some(sink) = pl.sink.as_ref() {
                    sink.set_volume(amp(pl.vol));
                }
            }
        });
    });

    // --- Play / pause ---------------------------------------------------
    ui.on_togglePlay({
        let weak = ui.as_weak();
        move || {
            let Some(ui) = weak.upgrade() else { return };

            let playing = PLAYER.with(|p| {
                let p = p.borrow();
                let sink = p.as_ref()?.sink.as_ref()?;
                if sink.empty() {
                    Some(false) // song already finished, nothing to toggle
                } else if sink.is_paused() {
                    sink.play();
                    Some(true)
                } else {
                    sink.pause();
                    Some(false)
                }
            });

            if let Some(v) = playing {
                ui.set_isPlaying(v);
            }
        }
    });

    // --- Seek (click on the progress bar) -------------------------------
    ui.on_seek(|frac: f32| {
        PLAYER.with(|p| {
            let p = p.borrow();
            if let Some(pl) = p.as_ref() {
                if let (Some(sink), Some(total)) = (pl.sink.as_ref(), pl.total) {
                    let target = total.mul_f32(frac.clamp(0.0, 1.0));
                    let _ = sink.try_seek(target); // some formats can't seek, that's fine
                }
            }
        });
    });

    // --- Close ----------------------------------------------------------
    ui.on_closeWindow({
        let weak = ui.as_weak();
        move || {
            if let Some(ui) = weak.upgrade() {
                let _ = ui.hide();
            }
        }
    });

    // --- Drag the frameless window around -------------------------------
    ui.on_startDrag({
        let weak = ui.as_weak();
        move || {
            if let Some(ui) = weak.upgrade() {
                ui.window().with_winit_window(|w| {
                    let _ = w.drag_window();
                });
            }
        }
    });

    // --- Ticks every 100ms: update the bar, move on when a song ends ----
    let timer = slint::Timer::default();
    timer.start(slint::TimerMode::Repeated, Duration::from_millis(100), {
        let weak = ui.as_weak();
        move || {
            let Some(ui) = weak.upgrade() else { return };

            let snap = PLAYER.with(|p| {
                let p = p.borrow();
                let pl = p.as_ref()?;
                let sink = pl.sink.as_ref()?;
                Some((sink.get_pos(), pl.total, sink.empty()))
            });
            let Some((pos, total, done)) = snap else { return };

            ui.set_elapsedText(fmtTime(pos).into());

            match total {
                Some(t) if !t.is_zero() => {
                    ui.set_totalText(fmtTime(t).into());
                    ui.set_progress((pos.as_secs_f32() / t.as_secs_f32()).min(1.0));
                }
                _ => {
                    ui.set_totalText("--:--".into());
                    ui.set_progress(0.0);
                }
            }

            // song ended -> play the next one, or stop if that was the last.
            // PLAYER isn't borrowed at this point so loading is safe.
            if done && ui.get_isPlaying() && !step(&ui, 1) {
                ui.set_isPlaying(false);
            }
        }
    });

    ui.run()
}