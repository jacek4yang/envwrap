pub mod cli;
pub mod compose;
pub mod exec;
pub mod output;
pub mod parser;
pub mod parser_dotenv;
pub mod parser_env;
pub mod parser_json;
pub mod proxy;
pub mod render;
pub mod util;
pub mod wrap;
pub use compose::Plan;
pub fn tool_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
