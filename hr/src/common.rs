//! Small helpers every part of the plugin uses.

use aether_sdk::prelude::*;

pub use aether_sdk::records::{id_of, is_off, pick, require, text, today, Record};

pub use aether_sdk::records::next_number;

/// A write that fails with a plain message for the caller when the kernel refuses it.
pub fn explain<T>(result: Result<T>, what: &str) -> Result<T> {
    result.map_err(|error| match error {
        Error::Message(message) => Error::Message(message),
        other => Error::msg(format!("{what}: {other}")),
    })
}
