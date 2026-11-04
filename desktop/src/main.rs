//! DroidLeap: move the mouse off a screen edge to use it, and the keyboard,
//! on an Android tablet connected over USB.

// Release builds on Windows are tray apps without a console window.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::{
    fs::{File, TryLockError},
    process::ExitCode,
};

use tokio::{
    sync::{mpsc, watch},
    task::LocalSet,
};

use droidleap::{
    config::{Config, Paths},
    engine::{Command, Engine, Status},
    log, logging, tray,
};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("droidleap: {e}");
            ExitCode::FAILURE
        }
    }
}

#[tokio::main(flavor = "current_thread")]
async fn run() -> Result<(), String> {
    let paths = Paths::new()?;
    let lock = File::create(&paths.lock_file)
        .map_err(|e| format!("{}: {e}", paths.lock_file.display()))?;
    lock.try_lock().map_err(|e| match e {
        TryLockError::WouldBlock => "already running".to_string(),
        TryLockError::Error(e) => format!("{}: {e}", paths.lock_file.display()),
    })?;
    logging::init(&paths.log_file).map_err(|e| format!("{}: {e}", paths.log_file.display()))?;
    log(format!("droidleap {} starting", env!("CARGO_PKG_VERSION")));

    let (config, problem) = match Config::load(&paths.config_file) {
        Ok(config) => (config, None),
        Err(e) => {
            log(format!("{e}; using defaults until it's fixed and reloaded"));
            (
                Config::default(),
                Some(format!("Settings file has an error: {e}")),
            )
        }
    };
    if !paths.config_file.exists()
        && let Err(e) = config.save(&paths.config_file)
    {
        log(e);
    }

    let (status_sender, status) = watch::channel(Status {
        problem,
        ..Status::new(&config)
    });
    let (commands, command_receiver) = mpsc::unbounded_channel();
    tray::spawn(status, commands.clone(), &paths);
    spawn_quit_on_signals(commands);

    // The Linux capture session runs as a local task.
    LocalSet::new()
        .run_until(Engine::new(config, paths, command_receiver, status_sender).run())
        .await;
    drop(lock);
    Ok(())
}

/// Ctrl+C, and SIGTERM at logout on Linux, quit cleanly so the tablet forgets our keyboard and mouse.
fn spawn_quit_on_signals(commands: mpsc::UnboundedSender<Command>) {
    tokio::spawn(async move {
        loop {
            if let Err(e) = quit_signal().await {
                log(format!("cannot listen for quit signals: {e}"));
                // Keeps `commands` alive: if the tray is gone, the engine must not see a closed channel.
                std::future::pending::<()>().await;
            }
            let _ = commands.send(Command::Quit);
        }
    });
}

async fn quit_signal() -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {
            result = tokio::signal::ctrl_c() => result,
            _ = terminate.recv() => Ok(()),
        }
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c().await
}
