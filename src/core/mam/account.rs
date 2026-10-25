//! The account as MAM reports it, the invariants read from it, and bonus point
//! spending (B-0011).

use std::time::{Duration, Instant, SystemTime};

use serde::Serialize;

use super::{ListedClient, MamFailure, Points, Purchase, UploadCredit, UserData};
use crate::core::{ByteSize, ClientVersion};

/// How often the account is read.
pub const ACCOUNT_READ_EVERY: Duration = Duration::from_mins(15);
/// How often the client list is included; MAM caches it for 30 minutes.
pub const CLIENT_STATS_EVERY: Duration = Duration::from_mins(30);
/// A reading this recent may admit a download or allow a purchase.
pub const ACCOUNT_FRESH_FOR: Duration = Duration::from_mins(5);
/// Unsatisfied slots kept free below MAM's limit.
pub const UNSATISFIED_RESERVE: u32 = 2;
/// Uploaded minus downloaded is kept at least this high.
pub const UPLOAD_BUFFER: ByteSize = ByteSize(25 << 30);
/// Upload credit is bought to keep the ratio at this, in thousandths.
pub const RATIO_TARGET: u64 = 3000;
/// Below this ratio, in thousandths, every download is held.
pub const RATIO_FLOOR: u64 = 2000;
/// Below this ratio, in thousandths, the notice is urgent.
pub const RATIO_URGENT: u64 = 1100;
/// VIP is renewed when less than this remains.
pub const VIP_RENEW_WITHIN: Duration = Duration::from_hours(14 * 24);
/// The owner is told when VIP ends within this and is not renewed.
pub const VIP_WARN_WITHIN: Duration = Duration::from_hours(7 * 24);
/// A VIP purchase fills the account to this long from the purchase.
pub const VIP_FILL: Duration = Duration::from_hours(90 * 24);
/// Upload credit is bought in steps of this many GiB.
pub const UPLOAD_STEP: u32 = 50;
/// Points per GiB of upload credit, as observed on 2026-10-24.
pub const UPLOAD_PRICE_PER_GIB: u64 = 500;
/// Points per week of VIP, as observed on 2026-10-24, until a purchase shows
/// another price.
pub const VIP_PRICE_PER_WEEK: u64 = 1250;
/// A refused purchase is not tried again for this long.
const REFUSAL_BACKOFF: Duration = Duration::from_hours(6);
/// A reading wanted early is not requested more often than this.
const EARLY_READ_SPACING: Duration = Duration::from_mins(1);
const WEEK: Duration = Duration::from_hours(7 * 24);

/// One reading of the account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reading {
    /// What MAM reported.
    pub data: UserData,
    /// When, by the calendar.
    pub at: SystemTime,
    /// When, by the process clock.
    pub taken: Instant,
}

/// MAM's client list, from the latest reading that included it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientList {
    /// The clients MAM lists.
    pub clients: Vec<ListedClient>,
    /// When it was read.
    pub at: SystemTime,
}

/// What MAM's client list says about the running qBittorrent (A5).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub enum ClientStanding {
    /// The list, or the running version, is not known yet.
    #[default]
    Unknown,
    /// MAM lists no client, as before anything seeds.
    NoneListed,
    /// MAM lists the running version and does not refuse it.
    Listed,
    /// MAM lists qBittorrent, but not the running version.
    NotListed,
    /// MAM marks the running version as not allowed.
    Refused,
    /// MAM lists clients in a form Windlass cannot match to qBittorrent.
    NotUnderstood,
}

impl ClientStanding {
    /// Whether downloads may start: MAM lists the client, or nothing yet.
    #[must_use]
    pub const fn admits(self) -> bool {
        matches!(self, Self::Listed | Self::NoneListed)
    }
}

/// How a purchase ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PurchaseOutcome {
    /// MAM sold it; the points left.
    Made(Points),
    /// MAM refused it.
    Refused(Option<String>),
    /// The outcome is unknown; nothing else is bought until the next reading
    /// settles it.
    Uncertain,
    /// An uncertain purchase, settled.
    Settled {
        /// Whether MAM sold it.
        made: bool,
        /// Who settled it.
        by: SettledBy,
    },
    /// The next reading could not tell whether MAM sold it; nothing else is
    /// bought until the owner checks on MAM.
    Unresolved,
}

/// Who settled an uncertain purchase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum SettledBy {
    /// The next reading: the points spent and what they bought.
    Reading,
    /// The owner, after checking on MAM.
    Owner,
}

/// What a purchase can change, as the reading before it showed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Before {
    points: Points,
    uploaded: ByteSize,
    vip_until: Option<SystemTime>,
}

/// The latest purchase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PurchaseRecord {
    /// What was bought.
    pub purchase: Purchase,
    /// When.
    pub at: SystemTime,
    /// How it ended, once it did.
    pub outcome: Option<PurchaseOutcome>,
    sent: Instant,
    before: Option<Before>,
    vip_hundredths: Option<u64>,
}

/// The account and spending.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    /// The latest reading.
    pub reading: Option<Reading>,
    /// When the next routine reading is due; now when absent.
    pub next_read: Option<Instant>,
    /// Readings that failed since the last success.
    pub failures: u32,
    /// Why the latest reading failed.
    pub last_failure: Option<MamFailure>,
    /// The latest purchase.
    pub last_purchase: Option<PurchaseRecord>,
    /// Points per week of VIP, from the last VIP purchase.
    pub vip_price_per_week: u64,
    /// MAM's client list.
    pub client_list: Option<ClientList>,
    client_stats_read: Option<Instant>,
    last_request: Option<Instant>,
    refused: Option<(Purchase, Instant)>,
}

impl Default for Account {
    fn default() -> Self {
        Self::new()
    }
}

impl Account {
    pub(super) const fn new() -> Self {
        Self {
            reading: None,
            next_read: None,
            failures: 0,
            last_failure: None,
            last_purchase: None,
            vip_price_per_week: VIP_PRICE_PER_WEEK,
            client_list: None,
            client_stats_read: None,
            last_request: None,
            refused: None,
        }
    }
}

/// What MAM reports about the torrents, read for the watches.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Attention {
    /// Whether MAM sees a partial download.
    pub partial: bool,
    /// Important tracker errors.
    pub tracker_errors: u32,
    /// Torrents in other categories MAM marks red, such as duplicate peers.
    pub flagged: u32,
}

impl Attention {
    /// Whether anything needs the owner.
    #[must_use]
    pub const fn any(&self) -> bool {
        self.partial || self.tracker_errors > 0 || self.flagged > 0
    }
}

impl Account {
    /// The latest reading's data.
    #[must_use]
    pub fn data(&self) -> Option<&UserData> {
        self.reading.as_ref().map(|reading| &reading.data)
    }

    /// The latest reading's data, if it is recent and taken after `since`.
    #[must_use]
    pub fn fresh(&self, now: Instant, since: Option<Instant>) -> Option<&UserData> {
        self.reading
            .as_ref()
            .filter(|reading| {
                now.saturating_duration_since(reading.taken) <= ACCOUNT_FRESH_FOR
                    && since.is_none_or(|since| reading.taken > since)
            })
            .map(|reading| &reading.data)
    }

    /// The reading to request now: routinely, or early when a decision needs a
    /// fresh one.
    pub(super) fn wanted(&self, now: Instant, early: bool) -> Option<bool> {
        let routine = self.next_read.is_none_or(|next| next <= now);
        let early = early
            && self
                .last_request
                .is_none_or(|last| now.saturating_duration_since(last) >= EARLY_READ_SPACING);
        (routine || early).then(|| {
            self.client_stats_read
                .is_none_or(|read| now.saturating_duration_since(read) >= CLIENT_STATS_EVERY)
        })
    }

    pub(super) const fn requested(&mut self, now: Instant) {
        self.last_request = Some(now);
    }

    /// When a routine reading or an early one may next be requested.
    pub(super) fn next_due(&self, early: bool) -> Option<Instant> {
        let early = early
            .then(|| {
                self.last_request
                    .and_then(|last| last.checked_add(EARLY_READ_SPACING))
            })
            .flatten();
        match (self.next_read, early) {
            (Some(next), Some(early)) => Some(next.min(early)),
            (next, early) => next.or(early),
        }
    }

    /// Store a reading; whether it settled an uncertain purchase, and how.
    pub(super) fn read(
        &mut self,
        now: Instant,
        wall: SystemTime,
        data: UserData,
    ) -> Option<(Purchase, PurchaseOutcome)> {
        let settled = self.settle(&data);
        if let Some(clients) = &data.clients {
            self.client_stats_read = Some(now);
            self.client_list = Some(ClientList {
                clients: clients.clone(),
                at: wall,
            });
        }
        self.reading = Some(Reading {
            data,
            at: wall,
            taken: now,
        });
        self.failures = 0;
        self.last_failure = None;
        self.next_read = now.checked_add(ACCOUNT_READ_EVERY);
        settled
    }

    /// Settle an uncertain purchase from the reading after it: points that
    /// were not spent mean MAM did not sell it; points spent and what they buy
    /// arrived mean it did. Anything else waits for the owner.
    fn settle(&mut self, after: &UserData) -> Option<(Purchase, PurchaseOutcome)> {
        let record = self
            .last_purchase
            .as_mut()
            .filter(|record| record.outcome == Some(PurchaseOutcome::Uncertain))?;
        let made = record.before.and_then(|before| match record.purchase {
            Purchase::Vip => Some(after.vip_until > before.vip_until),
            Purchase::Upload(amount) => {
                let gib = u64::from(amount.into_inner());
                let price = gib.saturating_mul(UPLOAD_PRICE_PER_GIB);
                let spent = before.points.0.saturating_sub(after.points.0);
                let credited = after.uploaded.0.saturating_sub(before.uploaded.0) >= gib << 30;
                match (spent >= price / 2, credited) {
                    (false, _) => Some(false),
                    (true, true) => Some(true),
                    (true, false) => None,
                }
            }
        });
        let outcome = made.map_or(PurchaseOutcome::Unresolved, |made| {
            PurchaseOutcome::Settled {
                made,
                by: SettledBy::Reading,
            }
        });
        record.outcome = Some(outcome.clone());
        Some((record.purchase, outcome))
    }

    /// The owner checked an unresolved purchase on MAM.
    pub(super) fn checked(&mut self, made: bool) -> Option<Purchase> {
        let record = self
            .last_purchase
            .as_mut()
            .filter(|record| record.outcome == Some(PurchaseOutcome::Unresolved))?;
        record.outcome = Some(PurchaseOutcome::Settled {
            made,
            by: SettledBy::Owner,
        });
        Some(record.purchase)
    }

    /// The purchase waiting for the owner, if one does.
    #[must_use]
    pub fn unresolved_purchase(&self) -> Option<Purchase> {
        self.last_purchase
            .as_ref()
            .filter(|record| record.outcome == Some(PurchaseOutcome::Unresolved))
            .map(|record| record.purchase)
    }

    pub(super) fn failed(&mut self, now: Instant, failure: MamFailure) {
        let wait = match &failure {
            MamFailure::TooSoon {
                retry_after: Some(after),
            } => ACCOUNT_FRESH_FOR.max(*after),
            _ => ACCOUNT_FRESH_FOR,
        };
        self.failures = self.failures.saturating_add(1);
        self.last_failure = Some(failure);
        self.next_read = now.checked_add(wait);
    }

    /// The entries MAM lists for the running qBittorrent version.
    pub fn running_client(
        &self,
        running: ClientVersion,
    ) -> impl Iterator<Item = &ListedClient> + '_ {
        self.client_list
            .iter()
            .flat_map(|list| &list.clients)
            .filter(move |client| client.qbittorrent == Some(running))
    }

    /// What MAM's client list says about the running qBittorrent.
    #[must_use]
    pub fn client_standing(&self, running: Option<ClientVersion>) -> ClientStanding {
        let (Some(list), Some(running)) = (&self.client_list, running) else {
            return ClientStanding::Unknown;
        };
        let ours: Vec<&ListedClient> = self.running_client(running).collect();
        if ours.iter().any(|client| client.refused) {
            ClientStanding::Refused
        } else if !ours.is_empty() {
            ClientStanding::Listed
        } else if list.clients.is_empty() {
            ClientStanding::NoneListed
        } else if list
            .clients
            .iter()
            .all(|client| client.qbittorrent.is_none())
        {
            ClientStanding::NotUnderstood
        } else {
            ClientStanding::NotListed
        }
    }

    /// The ratio in thousandths, if anything was downloaded.
    #[must_use]
    pub fn ratio(&self) -> Option<u64> {
        self.data().and_then(UserData::ratio_thousandths)
    }

    /// Whether the ratio is below the floor, and whether urgently.
    #[must_use]
    pub fn ratio_low(&self) -> Option<(u64, bool)> {
        self.ratio()
            .filter(|ratio| *ratio < RATIO_FLOOR)
            .map(|ratio| (ratio, ratio < RATIO_URGENT))
    }

    /// The unsatisfied count and limit, once 80% of the limit is reached.
    #[must_use]
    pub fn unsatisfied_near_limit(&self) -> Option<(u32, u32)> {
        let summary = self.data()?.summary.as_ref()?;
        let (count, limit) = (summary.unsatisfied, summary.unsatisfied_limit);
        (u64::from(count) * 5 >= u64::from(limit) * 4 && limit > 0).then_some((count, limit))
    }

    /// Unsatisfied torrents MAM sees not seeding.
    #[must_use]
    pub fn not_seeding_unsatisfied(&self) -> Option<u32> {
        let summary = self.data()?.summary.as_ref()?;
        Some(
            summary
                .unsatisfied_not_seeding
                .saturating_add(summary.hit_and_run_not_seeding),
        )
    }

    /// What else MAM flags. The not-seeding categories have their own watch.
    #[must_use]
    pub fn attention(&self) -> Option<Attention> {
        let summary = self.data()?.summary.as_ref()?;
        Some(Attention {
            partial: summary.partial,
            tracker_errors: summary.tracker_errors,
            flagged: summary
                .red
                .iter()
                .filter(|category| !category.name.starts_with("Not Seeding"))
                .map(|category| category.count)
                .fold(0, u32::saturating_add),
        })
    }

    /// When VIP ends, if within the warning period.
    #[must_use]
    pub fn vip_ending(&self, now: SystemTime) -> Option<SystemTime> {
        let until = self.data()?.vip_until?;
        let warned = now
            .checked_add(VIP_WARN_WITHIN)
            .is_some_and(|warn| until <= warn);
        (until > now && warned).then_some(until)
    }

    /// The purchase the spending policy wants now, if any. It needs a fresh
    /// reading taken after the last purchase.
    pub(super) fn wanted_purchase(
        &self,
        now: Instant,
        wall: SystemTime,
        reserve: Points,
    ) -> Option<Purchase> {
        let since = self.last_purchase.as_ref().map(|purchase| purchase.sent);
        let waiting = self.last_purchase.as_ref().is_some_and(|purchase| {
            matches!(
                purchase.outcome,
                None | Some(PurchaseOutcome::Uncertain | PurchaseOutcome::Unresolved)
            )
        });
        let data = self.fresh(now, since).filter(|_| !waiting)?;
        let affordable = |cost: u64| data.points.0 >= reserve.0.saturating_add(cost);
        let backed_off = |wanted: Purchase| {
            self.refused.is_some_and(|(refused, at)| {
                same_kind(refused, wanted) && now.saturating_duration_since(at) < REFUSAL_BACKOFF
            })
        };
        if let Some(cost) = self.vip_cost(data, wall)
            && affordable(cost)
            && !backed_off(Purchase::Vip)
        {
            return Some(Purchase::Vip);
        }
        let below_target = data.buffer() < UPLOAD_BUFFER
            || data
                .ratio_thousandths()
                .is_some_and(|ratio| ratio < RATIO_TARGET);
        let upload = Purchase::Upload(UploadCredit::try_new(UPLOAD_STEP).ok()?);
        (below_target
            && affordable(u64::from(UPLOAD_STEP) * UPLOAD_PRICE_PER_GIB)
            && !backed_off(upload))
        .then_some(upload)
    }

    /// The expected price of renewing VIP now, when it is due for renewal.
    #[must_use]
    pub fn vip_cost(&self, data: &UserData, now: SystemTime) -> Option<u64> {
        let hundredths = vip_hundredths(data, now)?;
        Some(
            hundredths
                .saturating_mul(self.vip_price_per_week)
                .div_ceil(100),
        )
    }

    pub(super) fn buying(&mut self, purchase: Purchase, now: Instant, wall: SystemTime) {
        self.last_purchase = Some(PurchaseRecord {
            purchase,
            at: wall,
            outcome: None,
            sent: now,
            before: self.data().map(|data| Before {
                points: data.points,
                uploaded: data.uploaded,
                vip_until: data.vip_until,
            }),
            vip_hundredths: self.data().and_then(|data| vip_hundredths(data, wall)),
        });
    }

    pub(super) fn bought(
        &mut self,
        purchase: Purchase,
        now: Instant,
        wall: SystemTime,
        outcome: PurchaseOutcome,
    ) {
        // A purchase is answered only after it was recorded as sent; the
        // fallback is never used.
        let record = self.last_purchase.get_or_insert(PurchaseRecord {
            purchase,
            at: wall,
            outcome: None,
            sent: now,
            before: None,
            vip_hundredths: None,
        });
        if let (PurchaseOutcome::Made(after), Purchase::Vip, Some(before), Some(hundredths)) = (
            &outcome,
            record.purchase,
            record.before,
            record.vip_hundredths,
        ) {
            let price =
                before.points.0.saturating_sub(after.0).saturating_mul(100) / hundredths.max(1);
            self.vip_price_per_week = Some(price)
                .filter(|price| *price > 0)
                .unwrap_or(self.vip_price_per_week);
        }
        if let PurchaseOutcome::Refused(_) = outcome {
            self.refused = Some((record.purchase, now));
        }
        record.outcome = Some(outcome);
    }
}

/// Hundredths of a week a VIP purchase would buy now, when renewal is due.
fn vip_hundredths(data: &UserData, now: SystemTime) -> Option<u64> {
    let remaining = data
        .vip_until
        .and_then(|until| until.duration_since(now).ok())
        .unwrap_or_default();
    if remaining >= VIP_RENEW_WITHIN {
        return None;
    }
    let bought = VIP_FILL.saturating_sub(remaining).as_secs();
    Some((bought.saturating_mul(100) / WEEK.as_secs()).max(100))
}

fn same_kind(left: Purchase, right: Purchase) -> bool {
    std::mem::discriminant(&left) == std::mem::discriminant(&right)
}
