use clap::Parser;
use ratio_mini::{Args, Event, run_session};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
fn main() {
    let args = Args::parse();
    let stop = Arc::new(AtomicBool::new(false));
    let signal = stop.clone();
    if let Err(error) = ctrlc::set_handler(move || signal.store(true, Ordering::SeqCst)) {
        eprintln!("Installation du gestionnaire Ctrl+C impossible : {error}");
        std::process::exit(1);
    }
    if let Err(error) = run_session(args, stop, |event| {
        if let Event::Log(message) = event {
            println!("{message}");
        }
    }) {
        eprintln!("Erreur : {error}");
        std::process::exit(1);
    }
}
