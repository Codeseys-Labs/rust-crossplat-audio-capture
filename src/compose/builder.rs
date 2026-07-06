//! Composition configuration: [`CompositionBuilder`], [`Group`],
//! [`GroupLayout`], and the [`ChannelMap`] describing the composed layout.

use std::time::Duration;

use crate::api::AudioCaptureBuilder;
use crate::core::capabilities::PlatformCapabilities;
use crate::core::config::CaptureTarget;
use crate::core::error::{AudioError, AudioResult};

use super::stream::Composition;

/// The maximum number of composed output channels. Mirrors the builder-side
/// ceiling `AudioCaptureBuilder` enforces for a single capture, so a composed
/// stream is never wider than any single capture is allowed to be.
pub(crate) const MAX_COMPOSED_CHANNELS: u16 = 32;

/// The maximum number of sources a composition may hold (config sanity bound —
/// each source is a full platform capture with its own ring and OS stream).
pub(crate) const MAX_SOURCES: usize = 16;

/// How a [`Group`]'s sources map onto the composed output channels.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupLayout {
    /// Fold every source in the group to mono (per-frame mean across the
    /// source's channels) and gain-weighted-sum them into **one** output
    /// channel.
    Mono,
    /// Fold every source in the group to stereo and gain-weighted-sum them
    /// into **two** output channels. Mono sources are duplicated to L/R;
    /// stereo sources pass through; wider sources are folded even→L / odd→R
    /// (per-side mean).
    Stereo,
    /// Pass the group's **single** source through with its native channel
    /// count (determined at [`Composition::start`]). v1 restriction: a
    /// keep-channels group must contain exactly one source.
    KeepChannels,
}

impl GroupLayout {
    /// The number of output channels this layout contributes, when knowable
    /// from the layout alone (`None` for [`KeepChannels`](Self::KeepChannels),
    /// whose width is the source's negotiated channel count).
    pub(crate) fn fixed_width(&self) -> Option<u16> {
        match self {
            GroupLayout::Mono => Some(1),
            GroupLayout::Stereo => Some(2),
            GroupLayout::KeepChannels => None,
        }
    }
}

/// One composition group: a named set of sources sharing a [`GroupLayout`].
///
/// Built fluently and handed to [`CompositionBuilder::group`]:
///
/// ```rust
/// use rsac::compose::{Group, GroupLayout};
/// use rsac::core::config::CaptureTarget;
///
/// let g = Group::new("voice")
///     .source(CaptureTarget::ApplicationByName("discord".into()))
///     .source_with_gain(CaptureTarget::ApplicationByName("zoom".into()), 0.8)
///     .mixdown(GroupLayout::Mono);
/// assert_eq!(g.sources().len(), 2);
/// ```
#[derive(Debug, Clone)]
pub struct Group {
    name: String,
    sources: Vec<(CaptureTarget, f32)>,
    layout: GroupLayout,
}

impl Group {
    /// Creates a group with the given name and the default
    /// [`GroupLayout::Stereo`] layout.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            sources: Vec::new(),
            layout: GroupLayout::Stereo,
        }
    }

    /// Adds a source with unit gain (1.0).
    pub fn source(self, target: CaptureTarget) -> Self {
        self.source_with_gain(target, 1.0)
    }

    /// Adds a source with an explicit linear gain applied during mixdown
    /// (1.0 = unity; validation requires the gain to be finite and ≥ 0).
    pub fn source_with_gain(mut self, target: CaptureTarget, gain: f32) -> Self {
        self.sources.push((target, gain));
        self
    }

    /// Sets the group's mixdown layout ([`Mono`](GroupLayout::Mono) or
    /// [`Stereo`](GroupLayout::Stereo)); for native-channel passthrough use
    /// [`keep_channels`](Self::keep_channels).
    pub fn mixdown(mut self, layout: GroupLayout) -> Self {
        self.layout = layout;
        self
    }

    /// Sets the group to [`GroupLayout::KeepChannels`]: its single source's
    /// native channels are appended to the composed output unchanged.
    pub fn keep_channels(mut self) -> Self {
        self.layout = GroupLayout::KeepChannels;
        self
    }

    /// The group's name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The group's `(target, gain)` pairs, in declaration order.
    pub fn sources(&self) -> &[(CaptureTarget, f32)] {
        &self.sources
    }

    /// The group's layout.
    pub fn layout(&self) -> GroupLayout {
        self.layout
    }
}

/// Where one composed output channel comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelOrigin {
    /// Name of the group this channel belongs to.
    pub group: String,
    /// Index of the group in declaration order.
    pub group_index: usize,
    /// Channel index *within* the group (0-based; e.g. 0 = L, 1 = R for a
    /// stereo group).
    pub channel_in_group: u16,
}

/// Maps composed output channels back to the groups that produce them.
///
/// Entry `i` describes interleaved output channel `i`. Group channels are
/// contiguous and appear in group declaration order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelMap {
    entries: Vec<ChannelOrigin>,
}

impl ChannelMap {
    pub(crate) fn new(entries: Vec<ChannelOrigin>) -> Self {
        Self { entries }
    }

    /// Total number of composed output channels.
    pub fn channels(&self) -> u16 {
        self.entries.len() as u16
    }

    /// Per-channel origins, indexed by output channel.
    pub fn entries(&self) -> &[ChannelOrigin] {
        &self.entries
    }

    /// The contiguous output-channel range contributed by the named group, or
    /// `None` if no group has that name.
    pub fn group_range(&self, group: &str) -> Option<std::ops::Range<usize>> {
        let start = self.entries.iter().position(|e| e.group == group)?;
        let end = start
            + self.entries[start..]
                .iter()
                .take_while(|e| e.group == group)
                .count();
        Some(start..end)
    }
}

/// Fully validated composition parameters, produced by
/// [`CompositionBuilder::build`] and consumed by [`Composition`].
#[derive(Debug, Clone)]
pub(crate) struct CompositionPlan {
    pub session_rate: u32,
    pub clamp_output: bool,
    pub quantum: Duration,
    pub stall_timeout: Duration,
    pub max_buffer: Duration,
    pub groups: Vec<Group>,
}

impl CompositionPlan {
    /// Frames per composed tick at the session rate (≥ 1).
    pub fn quantum_frames(&self) -> usize {
        let frames =
            (self.session_rate as u128 * self.quantum.as_nanos() / 1_000_000_000u128) as usize;
        frames.max(1)
    }

    /// Per-source FIFO bound in frames at the session rate (≥ one quantum).
    pub fn max_fifo_frames(&self) -> usize {
        let frames =
            (self.session_rate as u128 * self.max_buffer.as_nanos() / 1_000_000_000u128) as usize;
        frames.max(self.quantum_frames())
    }

    /// Index (in flat declaration order across all groups) of the master-clock
    /// source: the first `SystemDefault` / `Device` source, else source 0.
    pub fn master_index(&self) -> usize {
        let mut idx = 0usize;
        let mut first_system = None;
        for group in &self.groups {
            for (target, _gain) in group.sources() {
                if first_system.is_none()
                    && matches!(
                        target,
                        CaptureTarget::SystemDefault | CaptureTarget::Device(_)
                    )
                {
                    first_system = Some(idx);
                }
                idx += 1;
            }
        }
        first_system.unwrap_or(0)
    }
}

/// Builder for a multi-source [`Composition`] (ADR-0011).
///
/// See the [module docs](crate::compose) for the composition model and a full
/// example. Defaults: 48 kHz session rate, 10 ms tick quantum, 250 ms master
/// stall timeout, 1 s per-source buffering bound, no output clamping.
#[derive(Debug, Clone)]
pub struct CompositionBuilder {
    session_rate: u32,
    clamp_output: bool,
    quantum: Duration,
    stall_timeout: Duration,
    max_buffer: Duration,
    groups: Vec<Group>,
}

impl Default for CompositionBuilder {
    fn default() -> Self {
        Self {
            session_rate: 48_000,
            clamp_output: false,
            quantum: Duration::from_millis(10),
            stall_timeout: Duration::from_millis(250),
            max_buffer: Duration::from_secs(1),
            groups: Vec::new(),
        }
    }
}

impl CompositionBuilder {
    /// Creates a builder with default settings (48 kHz, no clamping, 10 ms
    /// quantum, 250 ms stall timeout, 1 s buffering bound, no groups).
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the session sample rate in Hz. Sources delivering a different rate
    /// are resampled to this rate. Must be one of
    /// [`PlatformCapabilities::SUPPORTED_SAMPLE_RATES`].
    pub fn sample_rate(mut self, rate: u32) -> Self {
        self.session_rate = rate;
        self
    }

    /// Enables/disables saturating output clamping to `[-1.0, 1.0]` after
    /// summation. Default **off**: plain summation may exceed unity, which is
    /// legal for f32 pipelines (and f32 WAV); clipping strategy is otherwise
    /// the consumer's decision.
    pub fn clamp_output(mut self, clamp: bool) -> Self {
        self.clamp_output = clamp;
        self
    }

    /// Sets the composed tick quantum (output buffer duration). Default 10 ms.
    /// Clamped to at least one frame at the session rate.
    pub fn quantum(mut self, quantum: Duration) -> Self {
        self.quantum = quantum;
        self
    }

    /// Sets how long the compositor waits for the master-clock source before
    /// emitting a wall-clock fallback tick (so a stalled master never freezes
    /// the session). Default 250 ms.
    pub fn stall_timeout(mut self, timeout: Duration) -> Self {
        self.stall_timeout = timeout;
        self
    }

    /// Sets the per-source buffering bound. A source drifting ahead of the
    /// master beyond this bound has its oldest samples trimmed (counted in
    /// [`SourceStats::trimmed_frames`](super::SourceStats::trimmed_frames)).
    /// Default 1 s; clamped to at least one quantum.
    pub fn max_buffer(mut self, bound: Duration) -> Self {
        self.max_buffer = bound;
        self
    }

    /// Appends a group. Groups contribute output channels in the order they
    /// are added.
    pub fn group(mut self, group: Group) -> Self {
        self.groups.push(group);
        self
    }

    /// Read-only view of the groups added so far.
    pub fn groups(&self) -> &[Group] {
        &self.groups
    }

    /// Runs every device-independent validation [`build`](Self::build)
    /// performs, without constructing anything.
    ///
    /// # Errors
    ///
    /// - [`AudioError::ConfigurationError`] — no groups; an empty group; a
    ///   duplicate/empty group name; a keep-channels group with ≠ 1 source; a
    ///   non-finite or negative gain; too many sources
    ///   (> 16); a fixed-width channel total exceeding 32; a zero quantum or
    ///   stall timeout.
    /// - [`AudioError::InvalidParameter`] — unsupported session sample rate.
    /// - [`AudioError::PlatformNotSupported`] — a target the current platform
    ///   cannot capture (same check as
    ///   [`AudioCaptureBuilder::preflight`](crate::api::AudioCaptureBuilder::preflight)).
    pub fn preflight(&self) -> AudioResult<()> {
        if self.groups.is_empty() {
            return Err(AudioError::ConfigurationError {
                message: "Composition requires at least one group".to_string(),
            });
        }
        if self.quantum.is_zero() {
            return Err(AudioError::ConfigurationError {
                message: "Composition quantum must be non-zero".to_string(),
            });
        }
        if self.stall_timeout.is_zero() {
            return Err(AudioError::ConfigurationError {
                message: "Composition stall_timeout must be non-zero".to_string(),
            });
        }

        // Session rate uses the same whitelist a single capture's builder
        // enforces, so the two contracts cannot drift.
        if !PlatformCapabilities::SUPPORTED_SAMPLE_RATES.contains(&self.session_rate) {
            return Err(AudioError::InvalidParameter {
                param: "sample_rate".into(),
                reason: format!(
                    "Unsupported session sample rate: {} Hz. Supported: {}",
                    self.session_rate,
                    PlatformCapabilities::supported_sample_rates_display()
                ),
            });
        }

        let mut seen_names: Vec<&str> = Vec::with_capacity(self.groups.len());
        let mut total_sources = 0usize;
        let mut fixed_channels = 0u32;
        for group in &self.groups {
            if group.name().is_empty() {
                return Err(AudioError::ConfigurationError {
                    message: "Group names must be non-empty".to_string(),
                });
            }
            if seen_names.contains(&group.name()) {
                return Err(AudioError::ConfigurationError {
                    message: format!("Duplicate group name: '{}'", group.name()),
                });
            }
            seen_names.push(group.name());

            if group.sources().is_empty() {
                return Err(AudioError::ConfigurationError {
                    message: format!("Group '{}' has no sources", group.name()),
                });
            }
            if group.layout() == GroupLayout::KeepChannels && group.sources().len() != 1 {
                return Err(AudioError::ConfigurationError {
                    message: format!(
                        "Group '{}' uses KeepChannels with {} sources; keep-channels groups \
                         must contain exactly one source (v1 restriction — mix additional \
                         sources via a Mono/Stereo group instead)",
                        group.name(),
                        group.sources().len()
                    ),
                });
            }

            total_sources += group.sources().len();

            // KeepChannels width is only known after start(); count the fixed
            // layouts now and re-check the full total at start().
            if let Some(w) = group.layout().fixed_width() {
                fixed_channels += u32::from(w);
            }

            for (target, gain) in group.sources() {
                if !gain.is_finite() || *gain < 0.0 {
                    return Err(AudioError::ConfigurationError {
                        message: format!(
                            "Source gain {} in group '{}' is invalid (must be finite and >= 0)",
                            gain,
                            group.name()
                        ),
                    });
                }
                // Per-target capability validation is delegated to the single
                // source of truth: the capture builder's own preflight.
                AudioCaptureBuilder::new()
                    .with_target(target.clone())
                    .sample_rate(self.session_rate)
                    .preflight()?;
            }
        }

        if total_sources > MAX_SOURCES {
            return Err(AudioError::ConfigurationError {
                message: format!(
                    "Composition has {} sources; the maximum is {}",
                    total_sources, MAX_SOURCES
                ),
            });
        }
        if fixed_channels > u32::from(MAX_COMPOSED_CHANNELS) {
            return Err(AudioError::ConfigurationError {
                message: format!(
                    "Composition would produce at least {} output channels; the maximum is {}",
                    fixed_channels, MAX_COMPOSED_CHANNELS
                ),
            });
        }

        Ok(())
    }

    /// Validates the configuration and constructs a (not yet started)
    /// [`Composition`].
    ///
    /// No devices are touched here; source captures are created and started by
    /// [`Composition::start`]. See [`preflight`](Self::preflight) for the
    /// errors this can raise.
    pub fn build(self) -> AudioResult<Composition> {
        self.preflight()?;
        Ok(Composition::from_plan(CompositionPlan {
            session_rate: self.session_rate,
            clamp_output: self.clamp_output,
            quantum: self.quantum,
            stall_timeout: self.stall_timeout,
            max_buffer: self.max_buffer,
            groups: self.groups,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::ProcessId;

    fn sys_group(name: &str) -> Group {
        Group::new(name)
            .source(CaptureTarget::SystemDefault)
            .mixdown(GroupLayout::Stereo)
    }

    #[test]
    fn empty_builder_rejected() {
        let err = CompositionBuilder::new().preflight().unwrap_err();
        assert!(matches!(err, AudioError::ConfigurationError { .. }));
    }

    #[test]
    fn minimal_valid_composition_passes_preflight() {
        CompositionBuilder::new()
            .group(sys_group("main"))
            .preflight()
            .expect("single stereo system group is valid");
    }

    #[test]
    fn empty_group_rejected() {
        let err = CompositionBuilder::new()
            .group(Group::new("empty"))
            .preflight()
            .unwrap_err();
        assert!(matches!(err, AudioError::ConfigurationError { .. }));
    }

    #[test]
    fn empty_group_name_rejected() {
        let err = CompositionBuilder::new()
            .group(Group::new("").source(CaptureTarget::SystemDefault))
            .preflight()
            .unwrap_err();
        assert!(matches!(err, AudioError::ConfigurationError { .. }));
    }

    #[test]
    fn duplicate_group_names_rejected() {
        let err = CompositionBuilder::new()
            .group(sys_group("dup"))
            .group(sys_group("dup"))
            .preflight()
            .unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("Duplicate group name"), "got: {msg}");
    }

    #[test]
    fn keep_channels_requires_exactly_one_source() {
        let err = CompositionBuilder::new()
            .group(
                Group::new("keep")
                    .source(CaptureTarget::SystemDefault)
                    .source(CaptureTarget::SystemDefault)
                    .keep_channels(),
            )
            .preflight()
            .unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("KeepChannels"), "got: {msg}");

        // Exactly one source is fine.
        CompositionBuilder::new()
            .group(
                Group::new("keep")
                    .source(CaptureTarget::SystemDefault)
                    .keep_channels(),
            )
            .preflight()
            .expect("one-source keep group is valid");
    }

    #[test]
    fn invalid_gain_rejected() {
        for bad in [-0.5f32, f32::NAN, f32::INFINITY] {
            let err = CompositionBuilder::new()
                .group(
                    Group::new("g")
                        .source_with_gain(CaptureTarget::SystemDefault, bad)
                        .mixdown(GroupLayout::Mono),
                )
                .preflight()
                .unwrap_err();
            assert!(
                matches!(err, AudioError::ConfigurationError { .. }),
                "gain {bad} must be rejected"
            );
        }
    }

    #[test]
    fn unsupported_sample_rate_rejected() {
        let err = CompositionBuilder::new()
            .sample_rate(12345)
            .group(sys_group("main"))
            .preflight()
            .unwrap_err();
        assert!(matches!(
            err,
            AudioError::InvalidParameter { ref param, .. } if param == "sample_rate"
        ));
    }

    #[test]
    fn too_many_sources_rejected() {
        let mut group = Group::new("many").mixdown(GroupLayout::Mono);
        for _ in 0..(MAX_SOURCES + 1) {
            group = group.source(CaptureTarget::SystemDefault);
        }
        let err = CompositionBuilder::new()
            .group(group)
            .preflight()
            .unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("sources"), "got: {msg}");
    }

    #[test]
    fn zero_quantum_and_stall_timeout_rejected() {
        let err = CompositionBuilder::new()
            .quantum(Duration::ZERO)
            .group(sys_group("main"))
            .preflight()
            .unwrap_err();
        assert!(matches!(err, AudioError::ConfigurationError { .. }));

        let err = CompositionBuilder::new()
            .stall_timeout(Duration::ZERO)
            .group(sys_group("main"))
            .preflight()
            .unwrap_err();
        assert!(matches!(err, AudioError::ConfigurationError { .. }));
    }

    #[test]
    fn process_tree_target_delegates_to_capture_preflight() {
        // On platforms that support process-tree capture this passes; on ones
        // that don't it must surface PlatformNotSupported — either way the
        // decision comes from the capture builder's preflight, not compose.
        let result = CompositionBuilder::new()
            .group(
                Group::new("tree")
                    .source(CaptureTarget::ProcessTree(ProcessId(1)))
                    .mixdown(GroupLayout::Stereo),
            )
            .preflight();
        let caps = PlatformCapabilities::query();
        if caps.supports_process_tree_capture {
            result.expect("supported platform must pass");
        } else {
            assert!(matches!(
                result.unwrap_err(),
                AudioError::PlatformNotSupported { .. }
            ));
        }
    }

    #[test]
    fn quantum_frames_and_master_index() {
        let plan = CompositionPlan {
            session_rate: 48_000,
            clamp_output: false,
            quantum: Duration::from_millis(10),
            stall_timeout: Duration::from_millis(250),
            max_buffer: Duration::from_secs(1),
            groups: vec![
                Group::new("apps")
                    .source(CaptureTarget::ApplicationByName("x".into()))
                    .mixdown(GroupLayout::Mono),
                Group::new("sys")
                    .source(CaptureTarget::SystemDefault)
                    .keep_channels(),
            ],
        };
        assert_eq!(plan.quantum_frames(), 480);
        assert_eq!(plan.max_fifo_frames(), 48_000);
        // Master is the first SystemDefault/Device source in flat order — the
        // app source at index 0 is skipped in favor of the system source.
        assert_eq!(plan.master_index(), 1);
    }

    #[test]
    fn channel_map_group_range() {
        let map = ChannelMap::new(vec![
            ChannelOrigin {
                group: "voice".into(),
                group_index: 0,
                channel_in_group: 0,
            },
            ChannelOrigin {
                group: "sys".into(),
                group_index: 1,
                channel_in_group: 0,
            },
            ChannelOrigin {
                group: "sys".into(),
                group_index: 1,
                channel_in_group: 1,
            },
        ]);
        assert_eq!(map.channels(), 3);
        assert_eq!(map.group_range("voice"), Some(0..1));
        assert_eq!(map.group_range("sys"), Some(1..3));
        assert_eq!(map.group_range("nope"), None);
    }
}
