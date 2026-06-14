#[tokio::main]
async fn main() {
    mink_cli::cli::install_panic_hook();
    let args: Vec<String> = std::env::args().skip(1).collect();
    match mink_cli::cli::main_entry(args).await {
        Ok(exit) => {
            if exit.code != 0 {
                std::process::exit(exit.code);
            }
        }
        Err(e) => {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
    }
}
