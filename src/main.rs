//! # Beamium.
//!
//! Beamium scrap Prometheus endpoint and forward metrics to Warp10.
extern crate clap;
extern crate yaml_rust;
extern crate time;
extern crate hyper;
extern crate hyper_native_tls;
extern crate cast;
extern crate regex;
#[macro_use(o, slog_log, slog_trace, slog_debug, slog_info, slog_warn, slog_error, slog_crit)]
extern crate slog;
#[macro_use]
extern crate slog_scope;
extern crate slog_term;
extern crate slog_stream;
extern crate slog_json;
extern crate nix;

use clap::App;
use std::thread;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::fs;
use nix::sys::signal;
use std::time::Duration;

mod config;
mod source;
mod router;
mod sink;
mod log;

include!("version.rs");

static mut SIGINT: bool = false;

extern "C" fn handle_sigint(_: i32) {
    unsafe {
        SIGINT = true;
    }
}

/// Main loop.
fn main() {
    unsafe {
        let sig_action = signal::SigAction::new(signal::SigHandler::Handler(handle_sigint),
                                                signal::SaFlags::empty(),
                                                signal::SigSet::empty());
        signal::sigaction(signal::SIGINT, &sig_action).unwrap();
    }

    // Setup a bare logger
    log::bootstrap();

    let matches = App::new("beamium")
        .version(&*format!("{} ({})", env!("CARGO_PKG_VERSION"), COMMIT))
        .author("d33d33 <kevin@d33d33.fr>")
        .about("Send Prometheus metrics to Warp10")
        .args_from_usage("-c, --config=[FILE] 'Sets a custom config file'
                              \
                          -v...                'Increase verbosity level (console only)'")
        .get_matches();

    info!("starting");

    // Bootstrap config
    let config_path = matches.value_of("config").unwrap_or("");
    let config = config::load_config(&config_path);
    if config.is_err() {
        crit!("Fail to load config {}: {}",
              &config_path,
              config.err().unwrap());
        std::process::exit(-1);
    }
    let config = config.ok().unwrap();

    // Setup logging
    log::log(&config.parameters, matches.occurrences_of("v"));

    // Ensure dirs
    let dir = fs::create_dir_all(&config.parameters.source_dir);
    if dir.is_err() {
        crit!("Fail to create source directory {}: {}",
              &config.parameters.source_dir,
              dir.err().unwrap());
        std::process::exit(-1);
    }
    let dir = fs::create_dir_all(&config.parameters.sink_dir);
    if dir.is_err() {
        crit!("Fail to create sink directory {}: {}",
              &config.parameters.sink_dir,
              dir.err().unwrap());
        std::process::exit(-1);
    }

    // Synchronisation stuff
    // let signal = chan_signal::notify(&[Signal::INT, Signal::TERM]);
    let sigint = Arc::new(AtomicBool::new(false));
    let mut handles = Vec::with_capacity(config.sources.len() + 1 + config.sinks.len());

    // Spawn sources
    info!("spawning sources");
    for source in config.sources {
        let (parameters, sigint) = (config.parameters.clone(), sigint.clone());
        handles.push(thread::spawn(move || {
            slog_scope::scope(slog_scope::logger().new(o!("source" => source.name.clone())),
                              || source::source(&source, &parameters, sigint));
        }));
    }

    // Spawn router
    info!("spawning router");
    {
        let (sinks, labels, parameters, sigint) = (config.sinks.clone(),
                                                   config.labels.clone(),
                                                   config.parameters.clone(),
                                                   sigint.clone());
        handles.push(thread::spawn(move || {
            slog_scope::scope(slog_scope::logger().new(o!()),
                              || router::router(&sinks, &labels, &parameters, sigint));
        }));
    }

    // Spawn sinks
    info!("spawning sinks");
    for sink in config.sinks {
        let (parameters, sigint) = (config.parameters.clone(), sigint.clone());
        handles.push(thread::spawn(move || {
            slog_scope::scope(slog_scope::logger().new(o!("sink" => sink.name.clone())),
                              || sink::sink(&sink, &parameters, sigint));
        }));
    }

    info!("started");
    // Wait for sigint
    loop {
        thread::sleep(Duration::from_millis(10));

        unsafe {
            if SIGINT {
                sigint.store(true, Ordering::Relaxed);
            }
        }

        if sigint.load(Ordering::Relaxed) {
            break;
        }
    }

    info!("shutding down");
    for handle in handles {
        handle.join().unwrap();
    }
    info!("halted");
}
