#![warn(clippy::all, clippy::pedantic, clippy::nursery)]

mod client;
mod types;
mod write;

pub use client::QbitClient;
pub use types::{
    QbitAuthResult, QbitPortSyncResult, QbitPreferences, QbitTorrentDetails, QbitTorrentState,
    parse_mam_id,
};

#[cfg(test)]
mod tests;
