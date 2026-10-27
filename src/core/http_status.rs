//! The HTTP status a service answered with.

use derive_more::Display;
use http::StatusCode;
use serde::{Serialize, Serializer};

/// An HTTP status a service answered with, shown as its number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Display)]
#[display("{}", _0.as_u16())]
pub struct HttpStatus(StatusCode);

impl HttpStatus {
    /// The status a service answered with.
    #[must_use]
    pub const fn new(status: StatusCode) -> Self {
        Self(status)
    }
}

impl Serialize for HttpStatus {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u16(self.0.as_u16())
    }
}
