//! `PAUSE_ON_START` env-var parser.
//!
//! Locked syntax per `docs/observability-37pre-checklist.md` §B8:
//!
//! - `None` or empty string → no pauses (every core runs).
//! - `"true"` or `"all"` (case-insensitive) → all eight cores pre-paused.
//! - Comma-separated list of lowercase core names (whitespace
//!   tolerated) → exactly those cores.
//! - Unknown token → `Err` so `main` can fail loudly at startup
//!   rather than silently ignore a typo.

use windlass_machine::CoreId;

/// Reasons the `PAUSE_ON_START` env value could be rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PauseOnStartError {
    /// A token did not name a known core.
    UnknownCore { token: String },
}

impl std::fmt::Display for PauseOnStartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownCore { token } => write!(
                f,
                "PAUSE_ON_START: unknown core '{token}'; valid: \
                 vpn, qbit, mam, db, disk, docker, domain (or 'true'/'all')"
            ),
        }
    }
}

impl std::error::Error for PauseOnStartError {}

/// Parse the env-var value into the list of cores to pre-pause.
///
/// # Errors
/// Returns [`PauseOnStartError::UnknownCore`] if any token in a
/// comma-separated list does not name a known core.
pub fn parse_pause_on_start(value: Option<&str>) -> Result<Vec<CoreId>, PauseOnStartError> {
    let Some(raw) = value.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(Vec::new());
    };
    let lower = raw.to_ascii_lowercase();
    if lower == "true" || lower == "all" {
        return Ok(CoreId::all().to_vec());
    }
    let mut cores = Vec::new();
    for token in raw.split(',') {
        let token_lc = token.trim().to_ascii_lowercase();
        let core = match token_lc.as_str() {
            "vpn" => CoreId::Vpn,
            "qbit" => CoreId::Qbit,
            "mam" => CoreId::Mam,
            "db" => CoreId::Db,
            "disk" => CoreId::Disk,
            "docker" => CoreId::Docker,
            "domain" => CoreId::Domain,
            "" => continue, // tolerate trailing/leading commas
            _ => {
                return Err(PauseOnStartError::UnknownCore { token: token_lc });
            }
        };
        cores.push(core);
    }
    Ok(cores)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unset_yields_empty() {
        assert!(parse_pause_on_start(None).unwrap().is_empty());
    }

    #[test]
    fn empty_string_yields_empty() {
        assert!(parse_pause_on_start(Some("")).unwrap().is_empty());
        assert!(parse_pause_on_start(Some("   ")).unwrap().is_empty());
    }

    #[test]
    fn true_pauses_all_cores() {
        let all = parse_pause_on_start(Some("true")).unwrap();
        assert_eq!(all.len(), 8);
    }

    #[test]
    fn all_alias_pauses_all_cores() {
        assert_eq!(parse_pause_on_start(Some("all")).unwrap().len(), 8);
    }

    #[test]
    fn case_insensitive_true() {
        assert_eq!(parse_pause_on_start(Some("TRUE")).unwrap().len(), 8);
        assert_eq!(parse_pause_on_start(Some("True")).unwrap().len(), 8);
    }

    #[test]
    fn comma_list_selects_named_cores() {
        let cs = parse_pause_on_start(Some("mam,qbit")).unwrap();
        assert_eq!(cs, vec![CoreId::Mam, CoreId::Qbit]);
    }

    #[test]
    fn comma_list_trims_whitespace() {
        let cs = parse_pause_on_start(Some("  mam ,  qbit  ")).unwrap();
        assert_eq!(cs, vec![CoreId::Mam, CoreId::Qbit]);
    }

    #[test]
    fn comma_list_case_insensitive() {
        let cs = parse_pause_on_start(Some("MAM,QBit,docker")).unwrap();
        assert_eq!(cs, vec![CoreId::Mam, CoreId::Qbit, CoreId::Docker]);
    }

    #[test]
    fn comma_list_tolerates_empty_tokens() {
        // Trailing comma, leading comma — should not error.
        let cs = parse_pause_on_start(Some("mam,,qbit,")).unwrap();
        assert_eq!(cs, vec![CoreId::Mam, CoreId::Qbit]);
    }

    #[test]
    fn unknown_token_is_fatal() {
        let err = parse_pause_on_start(Some("mam,bogus,qbit")).unwrap_err();
        assert_eq!(
            err,
            PauseOnStartError::UnknownCore {
                token: "bogus".into(),
            }
        );
    }
}
