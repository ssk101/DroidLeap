//! DroidLeap: move the mouse off a screen edge to use it, and the keyboard,
//! on an Android device connected over USB.

// Release builds on Windows are tray apps without a console window.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::{
    fs::{File, TryLockError},
    process::{ExitCode, Stdio},
};

use tokio::{
    sync::{mpsc, watch},
    task::LocalSet,
};

use droidleap::{
    config::{Config, Paths},
    engine::{Command, Engine, Status},
    log, logging, tray, update,
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
    // Where this app lives, from before an update replaces it (on Linux the path would then read "(deleted)").
    let exe =
        std::env::current_exe().map_err(|e| format!("cannot find the app's own file: {e}"))?;
    update::cleanup(&exe);
    let (update_requests, update_receiver) = mpsc::unbounded_channel();
    tray::spawn(status, commands.clone(), update_requests, &paths);
    tokio::spawn(update::run(update_receiver, commands.clone(), exe.clone()));
    spawn_quit_on_signals(commands);

    // The Linux capture session runs as a local task.
    let restart = LocalSet::new()
        .run_until(Engine::new(config, paths, command_receiver, status_sender).run())
        .await;
    drop(lock);
    if restart {
        log("starting the updated app");
        if let Err(e) = std::process::Command::new(&exe)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            log(format!("cannot start {}: {e}", exe.display()));
        }
    }
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
