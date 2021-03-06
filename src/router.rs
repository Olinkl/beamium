//! # Router module.
//!
//! The Router module forward sources to sinks.
use std::thread;
use std::time::Duration;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use time;
use std::cmp;
use std::collections::HashMap;
use std::io::prelude::*;
use std::fs;
use std::fs::File;
use std::error::Error;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use config;

/// Thread sleeping time.
const REST_TIME: u64 = 10;

/// Router loop.
pub fn router(sinks: &Vec<config::Sink>,
              labels: &HashMap<String, String>,
              parameters: &config::Parameters,
              sigint: Arc<AtomicBool>) {

    let labels: String = labels.iter()
        .fold(String::new(), |acc, (k, v)| {
            let sep = if acc.is_empty() { "" } else { "," };
            acc + sep + k + "=" + v
        });

    loop {
        let start = time::now_utc();

        match route(sinks, parameters, &labels) {
            Err(err) => error!("route fail: {}", err),
            Ok(_) => info!("route success"),
        }

        let elapsed = (time::now_utc() - start).num_milliseconds() as u64;
        let sleep_time = if elapsed > parameters.scan_period {
            REST_TIME
        } else {
            cmp::max(parameters.scan_period - elapsed, REST_TIME)
        };
        for _ in 0..sleep_time / REST_TIME {
            thread::sleep(Duration::from_millis(REST_TIME));
            if sigint.load(Ordering::Relaxed) {
                return;
            }
        }
    }
}

/// Route handle sources forwarding.
fn route(sinks: &Vec<config::Sink>,
         parameters: &config::Parameters,
         labels: &String)
         -> Result<(), Box<Error>> {
    debug!("route");
    loop {
        let entries = try!(fs::read_dir(&parameters.source_dir));
        let mut files = Vec::with_capacity(parameters.batch_count as usize);
        let mut metrics: Vec<String> = Vec::new();

        // Load metrics
        let mut batch_size = 0;
        for (i, entry) in entries.enumerate() {
            let entry = try!(entry);
            // Look only for metrics files
            if entry.path().extension() != Some(OsStr::new("metrics")) {
                continue;
            }

            // Split metrics in capped batch
            if i > parameters.batch_count as usize || batch_size > parameters.batch_size as usize {
                break;
            }

            debug!("open source file {}", format!("{:?}", entry.path()));
            let file = match read(entry.path()) {
                Err(err) => {
                    warn!(err);
                    continue;
                }
                Ok(v) => v,
            };

            for line in file.lines() {
                if labels.is_empty() {
                    metrics.push(String::from(line));
                    continue;
                }
                let mut parts = line.splitn(2, "{");

                let class = match parts.next() {
                    None => {
                        warn!("no_class");
                        continue;
                    }
                    Some(v) => v,
                };
                let class = String::from(class);
                let plabels = match parts.next() {
                    None => {
                        warn!("no_labels");
                        continue;
                    }
                    Some(v) => v,
                };
                let plabels = String::from(plabels);

                let slabels = labels.clone() +
                              if plabels.trim().starts_with("}") {
                    ""
                } else {
                    ","
                } + &plabels;

                metrics.push(format!("{}{{{}", class, slabels))
            }

            files.push(entry.path());
            batch_size += file.len();
        }

        // Nothing to do
        if files.len() == 0 {
            break;
        }

        // Setup sinks files
        let dir = Path::new(&parameters.sink_dir);
        {
            let mut sink_files = Vec::with_capacity(sinks.len() as usize);
            // Open tmp files
            for sink in sinks {
                let sink_file = dir.join(format!("{}.tmp", sink.name));
                debug!("open tmp sink file {}", format!("{:?}", sink_file));
                sink_files.push(try!(File::create(sink_file)));
            }

            // Write metrics
            debug!("write sink files");
            for line in metrics {
                if line.is_empty() {
                    continue;
                }

                for (i, sink) in sinks.iter().enumerate() {
                    if sink.selector.is_some() {
                        let selector = sink.selector.as_ref().unwrap();
                        if line.split_whitespace()
                            .nth(1)
                            .map_or(false, |class| selector.is_match(class)) {
                            continue;
                        }
                    }
                    try!(sink_files[i].write(line.as_bytes()));
                    try!(sink_files[i].write(b"\n"));
                }
            }

            // Flush
            for i in 0..sinks.len() {
                try!(sink_files[i].flush());
            }
        }

        // Rotate
        let now = time::now_utc().to_timespec().sec;
        for sink in sinks {
            let dest_file = dir.join(format!("{}-{}.metrics", sink.name, now));
            debug!("rotate tmp sink file to {}", format!("{:?}", dest_file));
            try!(fs::rename(dir.join(format!("{}.tmp", sink.name)), dest_file));
        }

        // Delete forwarded data
        for f in files {
            debug!("delete source file {}", format!("{:?}", f));
            try!(fs::remove_file(f));
        }
    }

    Ok(())
}

/// Read a file as String
fn read(path: PathBuf) -> Result<String, Box<Error>> {
    let mut file = try!(File::open(path));

    let mut content = String::new();
    try!(file.read_to_string(&mut content));

    Ok(content)
}
