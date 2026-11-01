/*
   ACE Tool

   Copyright 2026 Carnegie Mellon University.

   NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
   FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER
   EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR
   PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE
   MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT,
   TRADEMARK, OR COPYRIGHT INFRINGEMENT.

   Licensed under a MIT (SEI)-style license, please see license.txt or contact
   permission@sei.cmu.edu for full terms.

   [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
   distribution.  Please see Copyright notice for non-US Government use and distribution.

   This Software includes and/or makes use of Third-Party Software each subject to its own license.

   DM26-0670
*/

//! The broker's memory budget at run time (docs/architecture/cassini-backpressure.md, "Runtime budget"): set from
//! what the host and the container show, and from how fast the subscribers drain, rather than fixed at start.
//!
//! - **The floor** is a multiple of the largest frame ([`Tunables::floor_frames`], four): a broker that cannot have it
//!   at start does not start, saying so ([`startup_check`]); one whose memory later falls below it stops admitting (the
//!   budget's alarm) until memory returns, and says so, without exiting.
//! - **The ceiling** is what memory allows, sampled about every second through a [`MemorySource`]: a share of the
//!   host's `MemAvailable` above a reserve; the container's `memory.max` less the broker's working memory outside the
//!   budget; and, cut multiplicatively, pressure (PSI `some avg10` over a threshold, new `high` or `oom_kill` events),
//!   when the kernel shows them (PSI and the events file are optional: a nested rootless cgroup has neither).
//! - **The target** is Little's law: the smoothed drain rate (bytes delivered and acknowledged a second) times a
//!   latency target, the stall bound by default: holding more than the subscribers drain within the stall bound buys
//!   backlog, not throughput, as the stall rule acts at the bound.
//! - **The budget** follows the target, clipped to the floor, the ceiling and the cap (`CASSINI_MEMORY_BUDGET_BYTES`):
//!   it grows by a step while it is used and there is no pressure, falls by a step when it is well over the target, and
//!   is cut by a factor on pressure. A budget that shrinks below what is held evicts nothing: the budget's alarm stops
//!   admission until what is held drains.
//!
//! [`Controller`] is pure (it is given the time, the sample and the budget's counters), so its tests feed both.

use cassini_types::MAX_FRAME_BYTES;
use std::collections::VecDeque;
use std::time::Duration;

const MIB: u64 = 1024 * 1024;

/// What memory shows at one moment. Every field is optional: what the kernel or the container does not expose is
/// `None`, and is not used.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MemorySample {
    /// The host's (or the container's view of it) `MemAvailable`, in bytes.
    pub mem_available: Option<u64>,
    /// The container's `memory.max`, in bytes; `None` when it is `max` or cannot be read.
    pub limit: Option<u64>,
    /// The container's `memory.current`, in bytes.
    pub current: Option<u64>,
    /// PSI `some avg10` from `memory.pressure`, in percent.
    pub psi_some_avg10: Option<f64>,
    /// The `high` and `oom_kill` counts from `memory.events`.
    pub events_high: Option<u64>,
    pub events_oom_kill: Option<u64>,
}

/// Where samples come from: the real files, or a test's values.
pub trait MemorySource: Send + Sync {
    fn sample(&self) -> MemorySample;
}

/// The real memory: `/proc/meminfo` and the cgroup v2 files under `/sys/fs/cgroup`.
#[derive(Debug, Clone)]
pub struct SystemMemory {
    pub meminfo: String,
    pub cgroup: String,
}

impl Default for SystemMemory {
    fn default() -> Self {
        Self {
            meminfo: "/proc/meminfo".to_string(),
            cgroup: "/sys/fs/cgroup".to_string(),
        }
    }
}

impl MemorySource for SystemMemory {
    fn sample(&self) -> MemorySample {
        let read = |name: &str| std::fs::read_to_string(format!("{}/{name}", self.cgroup)).ok();
        let number = |text: Option<String>| text.and_then(|t| t.trim().parse::<u64>().ok());
        let events = read("memory.events");
        let event = |key: &str| {
            events.as_deref().and_then(|text| {
                text.lines()
                    .find_map(|line| line.strip_prefix(key)?.trim().parse::<u64>().ok())
            })
        };
        MemorySample {
            mem_available: std::fs::read_to_string(&self.meminfo)
                .ok()
                .and_then(|text| parse_mem_available(&text)),
            limit: number(read("memory.max")),
            current: number(read("memory.current")),
            psi_some_avg10: read("memory.pressure").and_then(|text| parse_psi_some_avg10(&text)),
            events_high: event("high "),
            events_oom_kill: event("oom_kill "),
        }
    }
}

/// `MemAvailable` from `/proc/meminfo` (given in kB), in bytes.
pub fn parse_mem_available(meminfo: &str) -> Option<u64> {
    meminfo.lines().find_map(|line| {
        let rest = line.strip_prefix("MemAvailable:")?;
        let kb = rest.split_whitespace().next()?.parse::<u64>().ok()?;
        Some(kb * 1024)
    })
}

/// `some avg10=` from a PSI file (`some avg10=1.23 avg60=… avg300=… total=…`).
pub fn parse_psi_some_avg10(pressure: &str) -> Option<f64> {
    pressure
        .lines()
        .find(|line| line.starts_with("some "))?
        .split_whitespace()
        .find_map(|field| field.strip_prefix("avg10=")?.parse::<f64>().ok())
}

/// Every tunable of the controller, with its default (marked for the maintainer's review in the doc).
#[derive(Debug, Clone, PartialEq)]
pub struct Tunables {
    /// How often memory is sampled and the budget set.
    pub sample_every: Duration,
    /// The floor, in largest frames.
    pub floor_frames: u64,
    /// The share of `MemAvailable` above the reserve the budget may grow into, as (numerator, denominator).
    pub available_share: (u64, u64),
    /// What the broker never pushes the host's `MemAvailable` below, in bytes.
    pub reserve: u64,
    /// PSI `some avg10` (percent) above which memory is under pressure.
    pub psi_threshold: f64,
    /// How much the budget grows, or falls toward the target, in one sample, in bytes.
    pub step: u64,
    /// What a pressure event leaves of the budget, as (numerator, denominator).
    pub cut: (u64, u64),
    /// The latency target, as a share of the stall bound (numerator, denominator): the target is the drain rate
    /// times it.
    pub latency_share: (u64, u64),
    /// How much of a new drain-rate measure the smoothed rate takes, as (numerator, denominator).
    pub smoothing: (u64, u64),
    /// The share of the budget held above which it counts as used, and may grow.
    pub used_share: (u64, u64),
    /// How far over the target, as a share of it, the budget may be before it falls toward it (hysteresis).
    pub over_target: (u64, u64),
}

impl Default for Tunables {
    fn default() -> Self {
        Self {
            sample_every: Duration::from_secs(1),
            floor_frames: 4,
            available_share: (1, 2),
            reserve: 512 * MIB,
            psi_threshold: 10.0,
            step: 16 * MIB,
            cut: (1, 2),
            latency_share: (1, 1),
            smoothing: (1, 4),
            used_share: (3, 4),
            over_target: (5, 4),
        }
    }
}

impl Tunables {
    /// The floor, in bytes.
    pub fn floor(&self) -> u64 {
        self.floor_frames * MAX_FRAME_BYTES as u64
    }
}

fn share(value: u64, (num, den): (u64, u64)) -> u64 {
    ((value as u128 * num as u128) / den.max(1) as u128) as u64
}

/// What memory allows at start: the lesser of the room under the container's limit and the host's `MemAvailable`.
pub fn available_at_start(sample: &MemorySample) -> Option<u64> {
    let under_limit = match (sample.limit, sample.current) {
        (Some(limit), current) => Some(limit.saturating_sub(current.unwrap_or(0))),
        (None, _) => None,
    };
    match (under_limit, sample.mem_available) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// Refuses a broker that memory cannot give its floor at start, or whose cap is under it, saying what it found and
/// what to give.
pub fn startup_check(
    sample: &MemorySample,
    tunables: &Tunables,
    cap: Option<u64>,
) -> Result<(), String> {
    let floor = tunables.floor();
    if let Some(cap) = cap
        && cap < floor
    {
        return Err(format!(
            "the broker's memory budget cap, {cap} bytes (CASSINI_MEMORY_BUDGET_BYTES), is below its floor of \
             {floor} bytes ({} frames of {MAX_FRAME_BYTES} bytes): give it at least {floor}, or no cap",
            tunables.floor_frames
        ));
    }
    if let Some(available) = available_at_start(sample)
        && available < floor
    {
        return Err(format!(
            "the broker needs at least {floor} bytes of memory for its budget ({} frames of {MAX_FRAME_BYTES} bytes) \
             and has {available} (memory.max {:?}, memory.current {:?}, MemAvailable {:?}): give the container at \
             least {floor} bytes more than its working memory",
            tunables.floor_frames, sample.limit, sample.current, sample.mem_available
        ));
    }
    Ok(())
}

/// What the ceiling came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CeilingKind {
    /// A share of the host's `MemAvailable`, above the reserve.
    MemAvailable,
    /// The container's `memory.max`, less the broker's working memory.
    Limit,
    /// Memory pressure cut the budget.
    Pressure,
}

/// What set the budget at the last sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitedBy {
    Floor,
    Ceiling(CeilingKind),
    Target,
    Cap,
}

/// A pressure event the controller saw.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PressureEvent {
    /// PSI `some avg10` over the threshold.
    Psi(f64),
    /// New `high` events in `memory.events`.
    High(u64),
    /// New `oom_kill` events in `memory.events`.
    OomKill(u64),
}

/// The controller's state, for pulled health (infrastructure introspection) and the periodic log line.
#[derive(Debug, Clone, PartialEq)]
pub struct BudgetSnapshot {
    pub budget: u64,
    pub ceiling: u64,
    pub target: u64,
    pub limited_by: LimitedBy,
    /// The smoothed drain rate, in bytes a second.
    pub drain_rate: u64,
    /// The last few pressure events, newest last, with how long after the controller started they came.
    pub recent_pressure: Vec<(Duration, PressureEvent)>,
    /// What memory allows is below the floor: the budget admits nothing until it returns.
    pub starved: bool,
    /// The budget's alarm: it admits nothing until what is held falls to its low water.
    pub alarm: bool,
    pub held: u64,
}

/// A learned value to start from: the last stable budget and the smoothed drain rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WarmStart {
    pub budget: u64,
    pub drain_rate: u64,
}

/// What one sample decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decision {
    pub budget: u64,
    /// Below the floor: admit nothing until memory returns.
    pub starved: bool,
}

const RECENT_PRESSURE: usize = 8;

/// The runtime budget controller. Pure: the caller gives it the time, memory's sample and the budget's counters.
#[derive(Debug, Clone)]
pub struct Controller {
    tunables: Tunables,
    stall: Duration,
    cap: Option<u64>,
    budget: u64,
    ceiling: u64,
    target: u64,
    limited_by: LimitedBy,
    drain_rate: u64,
    starved: bool,
    last: Option<(Duration, u64)>,
    events: (Option<u64>, Option<u64>),
    recent: VecDeque<(Duration, PressureEvent)>,
}

impl Controller {
    /// A controller starting at the floor (under the cap), as there is no demand yet to measure.
    pub fn new(tunables: Tunables, stall: Duration, cap: Option<u64>) -> Self {
        let floor = tunables.floor();
        Self {
            budget: floor,
            // Unknown until memory is first sampled: a warm start before then is clipped at the first sample.
            ceiling: u64::MAX,
            target: 0,
            limited_by: LimitedBy::Floor,
            drain_rate: 0,
            starved: false,
            last: None,
            events: (None, None),
            recent: VecDeque::new(),
            tunables,
            stall,
            cap,
        }
    }

    pub fn tunables(&self) -> &Tunables {
        &self.tunables
    }

    pub fn budget(&self) -> u64 {
        self.budget
    }

    /// Adopts a learned value, within the floor, the last ceiling and the cap; it goes on adjusting from there.
    pub fn warm_start(&mut self, learned: WarmStart) {
        self.drain_rate = learned.drain_rate;
        let (budget, limited_by) = self.clip(learned.budget);
        self.budget = budget;
        self.limited_by = limited_by;
    }

    /// The budget clipped to the floor, the ceiling and the cap, and what limited it (`Target` when nothing did).
    fn clip(&self, wanted: u64) -> (u64, LimitedBy) {
        let floor = self.tunables.floor();
        let mut budget = wanted;
        let mut limited_by = LimitedBy::Target;
        if budget >= self.ceiling {
            budget = self.ceiling;
            limited_by = self.limited_by_ceiling();
        }
        if let Some(cap) = self.cap
            && budget >= cap
            && cap < self.ceiling
        {
            budget = cap;
            limited_by = LimitedBy::Cap;
        }
        if budget < floor {
            budget = floor;
            limited_by = LimitedBy::Floor;
        }
        (budget, limited_by)
    }

    fn limited_by_ceiling(&self) -> LimitedBy {
        match self.limited_by {
            LimitedBy::Ceiling(kind) => LimitedBy::Ceiling(kind),
            _ => LimitedBy::Ceiling(CeilingKind::MemAvailable),
        }
    }

    /// One sample, `now` after the controller started: `held` bytes held, `refunded` bytes refunded since the start
    /// (what subscribers drained, and what holders let go). Returns the budget to set, and whether memory is below the
    /// floor.
    pub fn sample(
        &mut self,
        now: Duration,
        memory: &MemorySample,
        held: u64,
        refunded: u64,
    ) -> Decision {
        let t = self.tunables.clone();
        let floor = t.floor();

        // The drain rate, smoothed.
        if let Some((then, before)) = self.last
            && now > then
        {
            let seconds = (now - then).as_secs_f64();
            let measured = (refunded.saturating_sub(before) as f64 / seconds) as u64;
            let (num, den) = t.smoothing;
            self.drain_rate = ((self.drain_rate as u128 * (den - num) as u128
                + measured as u128 * num as u128)
                / den.max(1) as u128) as u64;
        }
        self.last = Some((now, refunded));
        self.target = (self.drain_rate as f64 * self.stall.as_secs_f64() * t.latency_share.0 as f64
            / t.latency_share.1.max(1) as f64) as u64;

        // The ceiling: the lesser of what the host and the container allow.
        let mut ceiling = u64::MAX;
        let mut kind = CeilingKind::MemAvailable;
        if let Some(available) = memory.mem_available {
            ceiling = held.saturating_add(share(
                available.saturating_sub(t.reserve),
                t.available_share,
            ));
        }
        if let (Some(limit), Some(current)) = (memory.limit, memory.current) {
            let working = current.saturating_sub(held);
            let under_limit = limit.saturating_sub(working);
            if under_limit < ceiling {
                ceiling = under_limit;
                kind = CeilingKind::Limit;
            }
        }

        // Pressure, when the kernel shows it.
        let mut pressured = false;
        if let Some(psi) = memory.psi_some_avg10
            && psi > t.psi_threshold
        {
            pressured = true;
            self.remember(now, PressureEvent::Psi(psi));
        }
        let (high_before, oom_before) = self.events;
        if let (Some(before), Some(high)) = (high_before, memory.events_high)
            && high > before
        {
            pressured = true;
            self.remember(now, PressureEvent::High(high - before));
        }
        if let (Some(before), Some(oom)) = (oom_before, memory.events_oom_kill)
            && oom > before
        {
            pressured = true;
            self.remember(now, PressureEvent::OomKill(oom - before));
        }
        self.events = (
            memory.events_high.or(high_before),
            memory.events_oom_kill.or(oom_before),
        );

        let mut wanted = self.budget;
        if pressured {
            wanted = share(self.budget, t.cut);
            if wanted < ceiling {
                ceiling = wanted;
                kind = CeilingKind::Pressure;
            }
        } else if held >= share(self.budget, t.used_share) && self.budget < self.target {
            wanted = (self.budget + t.step).min(self.target);
        } else if self.budget > share(self.target, t.over_target) {
            wanted = self.budget.saturating_sub(t.step).max(self.target);
        }

        self.starved = ceiling < floor;
        self.ceiling = ceiling.max(floor);
        self.limited_by = LimitedBy::Ceiling(kind);
        let (budget, limited_by) = self.clip(wanted);
        self.budget = budget;
        self.limited_by = if self.starved {
            LimitedBy::Floor
        } else {
            limited_by
        };
        Decision {
            budget,
            starved: self.starved,
        }
    }

    fn remember(&mut self, now: Duration, event: PressureEvent) {
        self.recent.push_back((now, event));
        while self.recent.len() > RECENT_PRESSURE {
            self.recent.pop_front();
        }
    }

    /// Its state, with the budget's alarm and what it holds.
    pub fn snapshot(&self, alarm: bool, held: u64) -> BudgetSnapshot {
        BudgetSnapshot {
            budget: self.budget,
            ceiling: self.ceiling,
            target: self.target,
            limited_by: self.limited_by,
            drain_rate: self.drain_rate,
            recent_pressure: self.recent.iter().copied().collect(),
            starved: self.starved,
            alarm,
            held,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const GIB: u64 = 1024 * MIB;
    const STALL: Duration = Duration::from_secs(30);

    fn plenty() -> MemorySample {
        MemorySample {
            mem_available: Some(48 * GIB),
            ..MemorySample::default()
        }
    }

    fn second(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn a_broker_below_its_floor_of_four_frames_does_not_start_and_says_why() {
        let t = Tunables::default();
        assert_eq!(t.floor(), 4 * MAX_FRAME_BYTES as u64);
        let short = MemorySample {
            mem_available: Some(48 * GIB),
            limit: Some(300 * MIB),
            current: Some(100 * MIB),
            ..MemorySample::default()
        };
        let refused = startup_check(&short, &t, None).unwrap_err();
        assert!(refused.contains(&t.floor().to_string()), "{refused}");
        assert!(
            refused.contains(&(200 * MIB).to_string()),
            "the room it has: {refused}"
        );
        let host = MemorySample {
            mem_available: Some(100 * MIB),
            ..MemorySample::default()
        };
        assert!(
            startup_check(&host, &t, None).is_err(),
            "the host's MemAvailable counts too"
        );
        let capped = startup_check(&plenty(), &t, Some(t.floor() - 1)).unwrap_err();
        assert!(capped.contains("CASSINI_MEMORY_BUDGET_BYTES"), "{capped}");
        assert!(startup_check(&plenty(), &t, None).is_ok());
        assert!(
            startup_check(&MemorySample::default(), &t, None).is_ok(),
            "nothing readable: nothing to refuse on"
        );
    }

    #[test]
    fn missing_pressure_and_event_files_are_no_pressure() {
        assert_eq!(parse_psi_some_avg10(""), None);
        assert_eq!(
            parse_psi_some_avg10(
                "some avg10=12.50 avg60=1.00 avg300=0.10 total=5\nfull avg10=0.00 avg60=0 avg300=0 total=0"
            ),
            Some(12.5)
        );
        assert_eq!(
            parse_mem_available("MemTotal: 65000000 kB\nMemAvailable:   51380224 kB\n"),
            Some(51_380_224 * 1024)
        );
        assert_eq!(parse_mem_available("MemTotal: 1 kB\n"), None);
        let mut c = Controller::new(Tunables::default(), STALL, None);
        let before = c.budget();
        for n in 0..5 {
            c.sample(second(n), &MemorySample::default(), 0, 0);
        }
        assert_eq!(c.budget(), before, "nothing known: it stays at the floor");
        assert!(c.snapshot(false, 0).recent_pressure.is_empty());
        // The real files, absent: a sample of nothing, not a failure.
        let nowhere = SystemMemory {
            meminfo: "/nowhere/meminfo".to_string(),
            cgroup: "/nowhere".to_string(),
        };
        assert_eq!(nowhere.sample(), MemorySample::default());
    }

    #[test]
    fn the_target_follows_the_drain_rate_and_quiet_use_grows_the_budget_toward_it() {
        let t = Tunables::default();
        let mut c = Controller::new(t.clone(), STALL, None);
        let rate = 40 * MIB; // drained a second
        let mut refunded = 0;
        for n in 0..60 {
            refunded += rate;
            // Held near the budget: it is used.
            let held = c.budget();
            c.sample(second(n), &plenty(), held, refunded);
        }
        let s = c.snapshot(false, 0);
        let expected = rate * STALL.as_secs();
        assert!(
            s.target > expected * 9 / 10 && s.target <= expected,
            "Little's law: the drain rate times the stall bound ({} of {expected})",
            s.target
        );
        assert!(
            c.budget() > t.floor(),
            "grown from the floor: {}",
            c.budget()
        );
        assert!(c.budget() <= s.target, "never past the target by growing");
        // Unused, it grows no more.
        let at = c.budget();
        c.sample(second(60), &plenty(), 0, refunded + rate);
        assert!(c.budget() <= at);
    }

    #[test]
    fn pressure_cuts_the_budget_and_quiet_lets_it_grow_again() {
        let t = Tunables::default();
        let mut c = Controller::new(t.clone(), STALL, None);
        c.warm_start(WarmStart {
            budget: 4 * GIB,
            drain_rate: 400 * MIB,
        });
        assert_eq!(c.budget(), 4 * GIB);
        let mut psi = plenty();
        psi.psi_some_avg10 = Some(t.psi_threshold + 1.0);
        c.sample(second(1), &psi, 4 * GIB, 0);
        assert_eq!(c.budget(), 2 * GIB, "cut by half");
        assert_eq!(
            c.snapshot(false, 0).limited_by,
            LimitedBy::Ceiling(CeilingKind::Pressure)
        );
        // New oom_kill events cut it too; a first reading of the counts is not news.
        let mut events = plenty();
        events.events_oom_kill = Some(3);
        events.events_high = Some(10);
        c.sample(second(2), &events, 2 * GIB, 400 * MIB);
        assert_eq!(
            c.budget(),
            2 * GIB + t.step,
            "first counts seen: no news, and used, it grows"
        );
        events.events_oom_kill = Some(4);
        c.sample(second(3), &events, 2 * GIB, 800 * MIB);
        assert_eq!(
            c.budget(),
            (2 * GIB + t.step) / 2,
            "a new oom_kill cuts it by half"
        );
        assert!(matches!(
            c.snapshot(false, 0).recent_pressure.last(),
            Some((_, PressureEvent::OomKill(1)))
        ));
        // Quiet and used, it grows by a step a sample.
        let before = c.budget();
        c.sample(second(4), &events, before, 1200 * MIB);
        assert_eq!(c.budget(), before + t.step);
    }

    /// The broker's container has a 1 GiB limit in the catalog (infra/config/workloads/cassini.nuon, 2026-11-01): the
    /// controller reads it from the cgroup's `memory.max`, the floor of four frames fits under it at start, and the
    /// ceiling is the limit less the broker's working memory outside the budget, not a share of it, and well under
    /// the share of the host's `MemAvailable` that bounded it without one.
    #[test]
    fn under_the_catalog_s_one_gib_limit_the_broker_starts_and_its_ceiling_is_the_limit_less_its_working_memory()
     {
        let t = Tunables::default();
        let dir = tempfile::tempdir().unwrap();
        let write = |name: &str, text: &str| std::fs::write(dir.path().join(name), text).unwrap();
        write("memory.max", "1073741824\n");
        write("memory.current", &format!("{}\n", 30 * MIB));
        // The host's, as a container sees /proc/meminfo: u7c's 49 GiB free.
        write(
            "meminfo",
            "MemTotal: 65011712 kB\nMemAvailable: 51380224 kB\n",
        );
        let memory = SystemMemory {
            meminfo: dir.path().join("meminfo").display().to_string(),
            cgroup: dir.path().display().to_string(),
        };
        let sample = memory.sample();
        assert_eq!(sample.limit, Some(GIB), "memory.max, read");
        assert_eq!(sample.current, Some(30 * MIB));
        assert_eq!(available_at_start(&sample), Some(GIB - 30 * MIB));
        assert!(
            startup_check(&sample, &t, None).is_ok(),
            "the floor, 256 MiB, fits"
        );

        let mut c = Controller::new(t.clone(), STALL, None);
        c.sample(second(0), &sample, 0, 0);
        let s = c.snapshot(false, 0);
        assert_eq!(
            s.ceiling,
            GIB - 30 * MIB,
            "the limit less the working memory"
        );
        assert!(
            s.ceiling < 49 * GIB / 2,
            "under the host's share, which was the ceiling without a limit"
        );
        // What the budget holds is in memory.current, and is not working memory: the ceiling stays where it was.
        let holding = MemorySample {
            current: Some(30 * MIB + 200 * MIB),
            ..sample.clone()
        };
        c.sample(second(1), &holding, 200 * MIB, 0);
        assert_eq!(c.snapshot(false, 200 * MIB).ceiling, GIB - 30 * MIB);

        // The floor holds at start only while the working memory leaves it room: 768 MiB of it leaves 256 MiB.
        let crowded = MemorySample {
            current: Some(800 * MIB),
            ..sample
        };
        assert!(startup_check(&crowded, &t, None).is_err());
    }

    #[test]
    fn below_the_floor_at_run_time_it_starves_and_recovers() {
        let t = Tunables::default();
        let mut c = Controller::new(t.clone(), STALL, None);
        let tight = MemorySample {
            mem_available: Some(t.reserve + 10 * MIB),
            ..MemorySample::default()
        };
        let d = c.sample(second(0), &tight, 0, 0);
        assert!(d.starved, "too little memory: admit nothing");
        assert_eq!(d.budget, t.floor(), "the budget is never below the floor");
        assert!(c.snapshot(false, 0).starved);
        let d = c.sample(second(1), &plenty(), 0, 0);
        assert!(!d.starved, "memory returned");
    }

    #[test]
    fn a_warm_start_is_clipped_taken_whenever_it_comes_and_its_absence_changes_nothing() {
        let t = Tunables::default();
        let cap = 2 * GIB;
        let cold = || Controller::new(t.clone(), STALL, Some(cap));
        let mut late = cold();
        let mut never = cold();
        for n in 0..3 {
            late.sample(second(n), &plenty(), 0, 0);
            never.sample(second(n), &plenty(), 0, 0);
        }
        assert_eq!(late.budget(), never.budget(), "before it comes, the same");
        late.warm_start(WarmStart {
            budget: 100 * GIB,
            drain_rate: 10 * MIB,
        });
        assert_eq!(late.budget(), cap, "clipped to the cap");
        let mut low = cold();
        low.warm_start(WarmStart {
            budget: 1,
            drain_rate: 0,
        });
        assert_eq!(low.budget(), t.floor(), "clipped to the floor");
        let mut tight = cold();
        let room = MemorySample {
            mem_available: Some(t.reserve + 2 * t.floor()),
            ..MemorySample::default()
        };
        tight.sample(second(0), &room, 0, 0);
        tight.warm_start(WarmStart {
            budget: cap,
            drain_rate: 0,
        });
        assert_eq!(
            tight.budget(),
            t.floor(),
            "clipped to the ceiling (half the room above the reserve)"
        );
        assert_eq!(cold().budget(), t.floor(), "absent: it starts at the floor");
    }

    fn any_sample() -> impl Strategy<Value = MemorySample> {
        (
            proptest::option::of(0u64..64 * GIB),
            proptest::option::of(0u64..8 * GIB),
            proptest::option::of(0u64..4 * GIB),
            proptest::option::of(0.0f64..100.0),
            proptest::option::of(0u64..20),
            proptest::option::of(0u64..5),
        )
            .prop_map(
                |(mem_available, limit, current, psi, high, oom)| MemorySample {
                    mem_available,
                    limit,
                    current,
                    psi_some_avg10: psi,
                    events_high: high,
                    events_oom_kill: oom,
                },
            )
    }

    proptest! {
        /// Whatever memory shows, the budget lies within the floor and, above it, the ceiling and the cap.
        #[test]
        fn the_budget_lies_within_its_floor_ceiling_and_cap(
            samples in proptest::collection::vec((any_sample(), 0u64..8 * GIB, 0u64..GIB), 1..30),
            cap in proptest::option::of(4 * MAX_FRAME_BYTES as u64..32 * GIB),
            warm in proptest::option::of((0u64..64 * GIB, 0u64..GIB)),
        ) {
            let t = Tunables::default();
            let mut c = Controller::new(t.clone(), STALL, cap);
            if let Some((budget, drain_rate)) = warm {
                c.warm_start(WarmStart { budget, drain_rate });
            }
            let mut refunded = 0;
            for (n, (sample, held, drained)) in samples.iter().enumerate() {
                refunded += drained;
                let d = c.sample(second(n as u64), sample, *held, refunded);
                let s = c.snapshot(false, *held);
                prop_assert!(d.budget >= t.floor());
                prop_assert!(d.budget <= s.ceiling.max(t.floor()));
                if let Some(cap) = cap {
                    prop_assert!(d.budget <= cap);
                }
                prop_assert_eq!(d.budget, s.budget);
            }
        }
    }
}
