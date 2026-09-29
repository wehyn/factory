// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    if std::env::args().any(|argument| argument == "--factory-mcp") {
        if let Err(error) = factory_core::mcp::serve_stdio_from_env() {
            let safe = factory_core::RedactedOutput::new(error.to_string());
            eprintln!("factory MCP server stopped: {}", safe.as_str());
            std::process::exit(1);
        }
        return;
    }
    agentic_factory_lib::run()
}
