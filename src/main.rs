use std::error::Error;

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    if let Some(data_dir) = cronos_ai::cli::run()? {
        cronos_ai::tui::run(data_dir)?;
    }
    Ok(())
}
