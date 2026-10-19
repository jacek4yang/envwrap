mod broker;
mod callback;
pub mod cli;
mod dns;
mod identity;
mod kernel;
mod network;
mod profile;
mod relay;
mod runtime;
mod transport;
mod upstream;
mod wire;

mod proxy;
pub fn tool_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
