use super::generic_proto;
use super::message::generic;
use core::fmt;
use std::error::Error;

pub mod buckets;
pub mod routetab;
pub mod protocol;
pub mod trans_proto;
// pub mod transpp;


/// An outbound ping failure.
#[derive(Debug)]
pub enum GroupFailure {
    /// The ping timed out, i.e. no response was received within the
    /// configured ping timeout.
    Timeout,
    /// The ping failed for reasons other than a timeout.
    Other { error: Box<dyn std::error::Error + Send + 'static> }
}

impl fmt::Display for GroupFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GroupFailure::Timeout => f.write_str("Ping timeout"),
            GroupFailure::Other { error } => write!(f, "Ping error: {}", error)
        }
    }
}

impl Error for GroupFailure {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            GroupFailure::Timeout => None,
            GroupFailure::Other { error } => Some(&**error)
        }
    }
}

#[cfg(test)]
mod test;