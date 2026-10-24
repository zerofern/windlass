//! The API session, the IP MAM sees, and the dynamic seedbox registration
//! (B-0004).

use std::{
    net::IpAddr,
    time::{Duration, Instant, SystemTime},
};

use super::{Asn, IpObservation, MamFailure, SessionRejection};

/// How often the IP MAM sees is observed; MAM allows once a minute.
pub const OBSERVE_EVERY: Duration = Duration::from_mins(5);
/// MAM allows one seedbox registration per rolling hour, whatever its result.
pub const REGISTRATION_SPACING: Duration = Duration::from_hours(1);

/// What Windlass knows about the API session.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Session {
    /// No request has proved the cookie since startup, a pause, or a change.
    #[default]
    Unverified,
    /// MAM accepted the cookie.
    Verified {
        /// When it last did.
        at: SystemTime,
    },
    /// MAM refused the cookie; requests that need it stop.
    Rejected {
        /// Why.
        rejection: SessionRejection,
        /// The IP and ASN observed when MAM refused it.
        seen: Option<(IpAddr, Asn)>,
        /// When.
        at: SystemTime,
    },
}

impl Session {
    /// Whether requests that need the cookie must wait. A refusal tied to the
    /// IP or ASN lifts once MAM sees a different one; any other waits for a new
    /// cookie.
    #[must_use]
    pub fn blocks(&self, observed: Option<&IpObservation>) -> bool {
        let Self::Rejected {
            rejection, seen, ..
        } = self
        else {
            return false;
        };
        let now = observed.map(|observed| (observed.ip, observed.asn));
        match rejection {
            SessionRejection::AsnMismatch => {
                now.is_none_or(|(_, asn)| seen.is_some_and(|(_, seen)| seen == asn))
            }
            SessionRejection::IpMismatch => {
                now.is_none_or(|(ip, _)| seen.is_some_and(|(seen, _)| seen == ip))
            }
            SessionRejection::InvalidCookie
            | SessionRejection::NotAllowed
            | SessionRejection::Other => true,
        }
    }
}

/// The IP and ASN MAM saw, and when.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observed {
    /// What MAM saw.
    pub observation: IpObservation,
    /// When.
    pub at: SystemTime,
    /// Whether it still describes the connection; a pause, lost protection, or
    /// a server switch makes it unconfirmed.
    pub current: bool,
}

/// Observing the IP MAM sees.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Network {
    /// The latest observation.
    pub observed: Option<Observed>,
    /// When the next lookup is due; now when absent.
    pub next_lookup: Option<Instant>,
    /// Lookups that failed since the last success.
    pub failures: u32,
    /// Why the latest lookup failed.
    pub last_failure: Option<MamFailure>,
}

impl Network {
    pub(super) const fn new() -> Self {
        Self {
            observed: None,
            next_lookup: None,
            failures: 0,
            last_failure: None,
        }
    }

    /// The current observation, if it still describes the connection.
    #[must_use]
    pub fn current(&self) -> Option<&IpObservation> {
        self.observed
            .as_ref()
            .filter(|observed| observed.current)
            .map(|observed| &observed.observation)
    }

    pub(super) fn due(&self, now: Instant) -> bool {
        self.next_lookup.is_none_or(|next| next <= now)
    }

    pub(super) fn observed(&mut self, now: Instant, wall: SystemTime, observation: IpObservation) {
        self.observed = Some(Observed {
            observation,
            at: wall,
            current: true,
        });
        self.failures = 0;
        self.last_failure = None;
        self.next_lookup = now.checked_add(OBSERVE_EVERY);
    }

    pub(super) fn failed(&mut self, now: Instant, failure: MamFailure) {
        let wait = match &failure {
            MamFailure::TooSoon {
                retry_after: Some(after),
            } => OBSERVE_EVERY.max(*after),
            _ => OBSERVE_EVERY,
        };
        self.failures = self.failures.saturating_add(1);
        self.last_failure = Some(failure);
        self.next_lookup = now.checked_add(wait);
    }

    /// The connection may have changed: observe again as soon as allowed.
    pub(super) const fn unconfirm(&mut self) {
        if let Some(observed) = &mut self.observed {
            observed.current = false;
        }
        self.next_lookup = None;
    }
}

/// What Windlass knows about the last registration attempt, which bounds the
/// next one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LastAttempt {
    /// Storage is not open yet.
    #[default]
    Unread,
    /// Reading it.
    Reading,
    /// Read: the last attempt, if there was one.
    Known(Option<SystemTime>),
    /// It could not be read; registration stays blocked.
    ReadFailed,
}

/// Why a registration did not confirm the seedbox IP.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistrationFailure {
    /// MAM answered with a failure, or the outcome is uncertain.
    Mam(MamFailure),
    /// The attempt could not be saved, so it was not sent.
    NotSaved,
}

/// The seedbox registration's standing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum SeedboxStatus {
    /// Not confirmed since startup, a pause, or a cookie change.
    #[default]
    Unconfirmed,
    /// MAM confirmed this IP as the seedbox.
    Confirmed {
        /// The registered IP.
        ip: IpAddr,
        /// When.
        at: SystemTime,
    },
    /// The latest attempt failed.
    Failed {
        /// Why.
        failure: RegistrationFailure,
        /// When.
        at: SystemTime,
    },
}

/// When the next registration may be sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistrationAllowance {
    /// Now.
    Now,
    /// Not before this time.
    After(SystemTime),
    /// Not until the last attempt is known.
    Unknown,
}

/// The dynamic seedbox registration.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Seedbox {
    /// The last attempt, which every later one must follow by an hour.
    pub last_attempt: LastAttempt,
    /// The registration's standing.
    pub status: SeedboxStatus,
    /// A later time MAM asked Windlass to wait until.
    pub not_before: Option<SystemTime>,
}

impl Seedbox {
    pub(super) const fn new() -> Self {
        Self {
            last_attempt: LastAttempt::Unread,
            status: SeedboxStatus::Unconfirmed,
            not_before: None,
        }
    }

    /// When the next attempt may be sent, as of `now`.
    #[must_use]
    pub fn allowance(&self, now: SystemTime) -> RegistrationAllowance {
        let LastAttempt::Known(last) = self.last_attempt else {
            return RegistrationAllowance::Unknown;
        };
        let spaced = last.and_then(|last| last.checked_add(REGISTRATION_SPACING));
        match spaced.max(self.not_before) {
            Some(allowed) if allowed > now => RegistrationAllowance::After(allowed),
            _ => RegistrationAllowance::Now,
        }
    }

    /// Whether MAM confirmed this IP as the seedbox.
    #[must_use]
    pub fn confirms(&self, observed: Option<&IpObservation>) -> bool {
        match (&self.status, observed) {
            (SeedboxStatus::Confirmed { ip, .. }, Some(observed)) => *ip == observed.ip,
            _ => false,
        }
    }

    /// Whether a registration should be sent now for the observed IP.
    pub(super) fn wanted(&self, now: SystemTime, observed: Option<&IpObservation>) -> bool {
        let refused = matches!(
            &self.status,
            SeedboxStatus::Failed {
                failure: RegistrationFailure::Mam(MamFailure::Session(
                    SessionRejection::NotAllowed
                )),
                ..
            }
        );
        observed.is_some()
            && !self.confirms(observed)
            && !refused
            && self.allowance(now) == RegistrationAllowance::Now
    }

    /// The attempt made at this time was saved; it is sent now, and every
    /// later attempt follows it by an hour.
    pub(super) const fn saved(&mut self, at: SystemTime) {
        self.last_attempt = LastAttempt::Known(Some(at));
    }

    pub(super) fn save_failed(&mut self, now: SystemTime) {
        self.status = SeedboxStatus::Failed {
            failure: RegistrationFailure::NotSaved,
            at: now,
        };
    }

    pub(super) fn confirmed(&mut self, ip: IpAddr, now: SystemTime) {
        self.status = SeedboxStatus::Confirmed { ip, at: now };
        self.not_before = None;
    }

    pub(super) fn failed(&mut self, failure: MamFailure, now: SystemTime) {
        if let MamFailure::TooSoon {
            retry_after: Some(after),
        } = &failure
        {
            self.not_before = now.checked_add(*after);
        }
        self.status = SeedboxStatus::Failed {
            failure: RegistrationFailure::Mam(failure),
            at: now,
        };
    }

    /// A pause, lost protection, or a new cookie needs fresh confirmation; the
    /// spacing from the last attempt remains.
    pub(super) fn unconfirm(&mut self) {
        self.status = SeedboxStatus::Unconfirmed;
    }
}
