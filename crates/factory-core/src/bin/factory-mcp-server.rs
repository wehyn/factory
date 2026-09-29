fn main() {
    if let Err(error) = factory_core::mcp::serve_stdio_from_env() {
        let safe = factory_core::RedactedOutput::new(error.to_string());
        eprintln!("factory MCP server stopped: {}", safe.as_str());
        std::process::exit(1);
    }
}
