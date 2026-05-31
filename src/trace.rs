//! Internal structured-instrumentation shim for non-RT lifecycle events.
//!
//! rsac instruments only high-value, **non-real-time** lifecycle points (build,
//! start, stop, drop, backpressure transitions). The two macros exported here —
//! `rsac_event!` and `rsac_span!` — give exactly one instrumentation call site
//! per event regardless of whether the optional `tracing` feature is enabled:
//!
//! - With `--features tracing`, they expand to `tracing::event!` /
//!   `tracing::span!`, so structured fields are recorded by any installed
//!   `tracing` subscriber.
//! - Without the feature (the default), they fall back to the equivalent
//!   `log::` macro, preserving the crate's existing zero-extra-dependency
//!   logging behavior. Structured key/value fields are flattened into the log
//!   message so no information is silently dropped.
//!
//! # RT-safety
//!
//! These macros are for the **control plane only**. They must never appear on
//! the OS audio callback / sample-push path (`audio/*/thread.rs`,
//! `bridge::ring_buffer` producer) — emitting a `tracing`/`log` record can
//! allocate, lock, or block, violating the real-time guarantee documented in
//! [`docs/designs/0001-rt-allocation-guarantee.md`](https://github.com/Codeseys-Labs/rust-crossplat-audio-capture/blob/master/docs/designs/0001-rt-allocation-guarantee.md).
//!
//! # Levels
//!
//! Both macros accept a leading level token — one of `error`, `warn`, `info`,
//! `debug`, or `trace` — mirroring the `log` / `tracing` level vocabulary.

/// Install a process-global `tracing` subscriber with sane defaults.
///
/// Convenience for binaries and examples that want rsac's lifecycle spans/events
/// rendered without wiring up a subscriber by hand. It is a best-effort,
/// idempotent call: if a global subscriber is already set (by the application
/// or a previous call), the error is swallowed and `false` is returned.
///
/// Returns `true` if this call installed the subscriber, `false` if one was
/// already present.
///
/// Library code never calls this; choosing and configuring a subscriber is the
/// application's responsibility. Available only with the `tracing` feature.
#[cfg(feature = "tracing")]
pub fn install_default_tracing() -> bool {
    let installed = tracing::subscriber::set_global_default(tracing_subscriber_fallback())
        .map(|()| true)
        .unwrap_or(false);
    // Example non-RT lifecycle event (control plane only — safe to allocate here).
    crate::rsac_event!(
        debug,
        installed = installed,
        "rsac default tracing subscriber"
    );
    installed
}

/// Build the default subscriber used by [`install_default_tracing`].
///
/// We intentionally avoid taking a dependency on `tracing-subscriber`: the
/// optional dependency surface is kept to just `tracing` itself. This returns
/// `tracing`'s built-in no-op subscriber, which is enough to make the global
/// dispatcher non-default (so events are dispatched rather than discarded by the
/// `NoSubscriber` fast path) while leaving formatting/filtering to whatever the
/// embedding application installs. Applications that want rendered output should
/// install their own `tracing-subscriber` registry and *not* call
/// [`install_default_tracing`].
#[cfg(feature = "tracing")]
fn tracing_subscriber_fallback() -> impl tracing::Subscriber + Send + Sync {
    tracing::subscriber::NoSubscriber::default()
}

/// Emit a structured lifecycle **event** at the given level.
///
/// Syntax mirrors `tracing::event!` / `log::log!`. To stay identical under both
/// backends, use only the portable `field = value` form (values are captured by
/// `Debug` in the `log` fallback). Do **not** use `tracing`'s `%`/`?` field
/// sigils here — they don't exist in the `log` fallback and would only compile
/// with the feature on.
///
/// ```ignore
/// rsac_event!(info, capture_id = id, "capture built");
/// rsac_event!(warn, "stop failed: {e}");
/// ```
///
/// With the `tracing` feature this becomes a `tracing::event!`; otherwise it
/// becomes the matching `log::<level>!`, with any structured fields prefixed to
/// the message so they survive the fallback.
#[macro_export]
macro_rules! rsac_event {
    // ── internal: tracing backend, level already resolved to a Level ident ──
    // `rust-analyzer`/rustc do NOT eagerly expand a normalizer macro in argument
    // position, so the public arms below resolve the level token themselves and
    // forward a concrete `ERROR`/`WARN`/… ident (matched here as `$lvl:ident`).
    (@__t $lvl:ident, $($rest:tt)+) => {
        $crate::trace::__rsac_tracing::event!($crate::trace::__rsac_tracing::Level::$lvl, $($rest)+)
    };

    // ── internal: log backend, as a tt-muncher ──────────────────────────────
    // A separated repetition `$($f:ident = $v:expr),+ , $($msg:tt)+` is locally
    // ambiguous (after an `expr`, `,` is in FOLLOW(expr) so the matcher cannot
    // tell a continuing field from the message tail). So we peel `field = val ,`
    // pairs one at a time into a bracketed accumulator `[ ... ]`; whatever no
    // longer parses as `ident = expr` is the trailing message. Fields are
    // rendered "key=value" so the `log::` fallback loses no information.
    //
    // Arm order is significant: peel-more before peel-last before message, and
    // empty-acc message before non-empty-acc message.
    //
    // 1) peel one `field = val ,` when more tokens follow.
    (@__lmunch $lvl:ident; [$($acc:tt)*]; $field:ident = $val:expr, $($rest:tt)+) => {
        $crate::rsac_event!(@__lmunch $lvl; [$($acc)* $field = $val,]; $($rest)+)
    };
    // 2) terminal: a final lone `field = val` (optional trailing comma), no message.
    (@__lmunch $lvl:ident; [$($acc:tt)*]; $field:ident = $val:expr $(,)?) => {
        $crate::trace::__rsac_log::$lvl!(
            "{}",
            $crate::__rsac_render_fields!($($acc)* $field = $val)
        )
    };
    // 3) terminal: message only (accumulator empty).
    (@__lmunch $lvl:ident; []; $($msg:tt)+) => {
        $crate::trace::__rsac_log::$lvl!($($msg)+)
    };
    // 4) terminal: fields + trailing message (accumulator non-empty).
    (@__lmunch $lvl:ident; [$($acc:tt)+]; $($msg:tt)+) => {
        $crate::trace::__rsac_log::$lvl!(
            "{} {}",
            $crate::__rsac_render_fields!($($acc)+),
            format_args!($($msg)+)
        )
    };

    // ── internal: dispatch on a resolved backend pair (tracing ident + log ident) ──
    (@__pick $t:ident / $l:ident, $($rest:tt)+) => {{
        #[cfg(feature = "tracing")]
        { $crate::rsac_event!(@__t $t, $($rest)+); }
        #[cfg(not(feature = "tracing"))]
        { $crate::rsac_event!(@__lmunch $l; []; $($rest)+); }
    }};

    // ── public entry: one arm per level token resolves both backend idents ──
    (error, $($rest:tt)+) => { $crate::rsac_event!(@__pick ERROR / error, $($rest)+) };
    (warn,  $($rest:tt)+) => { $crate::rsac_event!(@__pick WARN  / warn,  $($rest)+) };
    (info,  $($rest:tt)+) => { $crate::rsac_event!(@__pick INFO  / info,  $($rest)+) };
    (debug, $($rest:tt)+) => { $crate::rsac_event!(@__pick DEBUG / debug, $($rest)+) };
    (trace, $($rest:tt)+) => { $crate::rsac_event!(@__pick TRACE / trace, $($rest)+) };
}

/// Open a lifecycle **span** at the given level, returning a guard-like value.
///
/// With the `tracing` feature this expands to `tracing::span!`, yielding a
/// `tracing::Span` the caller can `.enter()` or `.in_scope(..)`. Without the
/// feature it expands to an event (spans have no `log` analogue) and yields the
/// unit value `()`, so call sites that simply create-and-hold a span still
/// compile and run identically under both configurations.
///
/// ```ignore
/// let _span = rsac_span!(info, "capture", capture_id = id);
/// let _enter = _span_enter(&_span); // see api.rs call sites
/// ```
#[macro_export]
macro_rules! rsac_span {
    // ── internal: dispatch on a resolved tracing Level ident ────────────────
    // With the feature off there is no `log` span analogue, so degrade to an
    // event (forwarding the original level token, kept as the `$orig` ident) and
    // yield (). With it on, build a real `tracing::Span`.
    (@__pick $t:ident / $orig:ident, $name:expr $(, $($rest:tt)+)?) => {{
        #[cfg(feature = "tracing")]
        {
            $crate::trace::__rsac_tracing::span!(
                $crate::trace::__rsac_tracing::Level::$t,
                $name
                $(, $($rest)+)?
            )
        }
        #[cfg(not(feature = "tracing"))]
        {
            $crate::rsac_event!($orig, $($($rest)+ ,)? "span: {}", $name);
        }
    }};

    // ── public entry: one arm per level token ───────────────────────────────
    (error, $name:expr $(, $($rest:tt)+)?) => { $crate::rsac_span!(@__pick ERROR / error, $name $(, $($rest)+)?) };
    (warn,  $name:expr $(, $($rest:tt)+)?) => { $crate::rsac_span!(@__pick WARN  / warn,  $name $(, $($rest)+)?) };
    (info,  $name:expr $(, $($rest:tt)+)?) => { $crate::rsac_span!(@__pick INFO  / info,  $name $(, $($rest)+)?) };
    (debug, $name:expr $(, $($rest:tt)+)?) => { $crate::rsac_span!(@__pick DEBUG / debug, $name $(, $($rest)+)?) };
    (trace, $name:expr $(, $($rest:tt)+)?) => { $crate::rsac_span!(@__pick TRACE / trace, $name $(, $($rest)+)?) };
}

/// Render `key = value` field lists into a single `String` for the `log`
/// fallback. Each value is formatted with `{:?}` (Debug) so heterogeneous field
/// types work without per-call-site sigils; this is the non-RT fallback path so
/// the allocation is acceptable.
#[doc(hidden)]
#[macro_export]
macro_rules! __rsac_render_fields {
    ($($field:ident = $val:expr),+ $(,)?) => {{
        // One small String on the non-RT control plane — never on the audio path.
        let mut __rsac_s = $crate::trace::__rsac_alloc::string::String::new();
        $(
            if !__rsac_s.is_empty() {
                $crate::trace::__rsac_alloc::fmt::Write::write_str(&mut __rsac_s, " ").ok();
            }
            $crate::trace::__rsac_alloc::fmt::Write::write_fmt(
                &mut __rsac_s,
                format_args!("{}={:?}", stringify!($field), &$val),
            )
            .ok();
        )+
        __rsac_s
    }};
}

// ── Re-export shims so the exported macros resolve `$crate::…` paths without
//    forcing call sites to import anything. ───────────────────────────────────

/// Internal: `log` re-export used by the `rsac_event!`/`rsac_span!` fallback.
#[doc(hidden)]
pub mod __rsac_log {
    pub use log::{debug, error, info, trace, warn};
}

/// Internal: `tracing` re-export used by the macros when the feature is on.
#[cfg(feature = "tracing")]
#[doc(hidden)]
pub mod __rsac_tracing {
    pub use tracing::{event, span, Level};
}

/// Internal: `alloc`/`core` items used by the field renderer.
#[doc(hidden)]
pub mod __rsac_alloc {
    pub mod string {
        pub use std::string::String;
    }
    pub mod fmt {
        pub use std::fmt::Write;
    }
}

#[cfg(test)]
mod tests {
    // The macros are #[macro_export]ed at the crate root, so reference them as
    // crate::rsac_event! / crate::rsac_span! here.

    #[test]
    fn event_without_message_compiles_and_runs() {
        let capture_id: u64 = 7;
        crate::rsac_event!(info, capture_id = capture_id);
    }

    #[test]
    fn event_with_fields_and_message_compiles_and_runs() {
        let capture_id: u64 = 42;
        let frames: usize = 480;
        crate::rsac_event!(debug, capture_id = capture_id, frames = frames, "built");
    }

    #[test]
    fn event_message_only_compiles_and_runs() {
        let e = "boom";
        crate::rsac_event!(warn, "stop failed: {e}");
    }

    #[test]
    fn event_all_levels_compile() {
        crate::rsac_event!(error, "e");
        crate::rsac_event!(warn, "w");
        crate::rsac_event!(info, "i");
        crate::rsac_event!(debug, "d");
        crate::rsac_event!(trace, "t");
    }

    #[test]
    // `rsac_span!` yields a real `tracing::Span` with the feature on and `()`
    // without; binding it exercises the create-and-hold call-site shape under
    // both configs. The unit value in the feature-off build is intentional.
    #[cfg_attr(not(feature = "tracing"), allow(clippy::let_unit_value))]
    fn span_with_fields_compiles_and_runs() {
        let capture_id: u64 = 99;
        // Hold the span value: a real tracing::Span with the feature on, () without.
        let _span = crate::rsac_span!(info, "capture", capture_id = capture_id);
    }

    #[test]
    #[cfg_attr(not(feature = "tracing"), allow(clippy::let_unit_value))]
    fn span_name_only_compiles_and_runs() {
        let _span = crate::rsac_span!(debug, "lifecycle");
    }

    #[cfg(not(feature = "tracing"))]
    #[test]
    fn field_renderer_orders_and_separates() {
        let id: u64 = 3;
        let name = "alpha";
        let rendered = crate::__rsac_render_fields!(capture_id = id, name = name);
        assert_eq!(rendered, "capture_id=3 name=\"alpha\"");
    }

    #[cfg(feature = "tracing")]
    #[test]
    fn install_default_tracing_is_idempotent_and_returns_bool() {
        // Either this call installs it (true) or something already did (false).
        // Calling twice must never panic and the second call must report false
        // because a global default is now present.
        let _first = crate::trace::install_default_tracing();
        let second = crate::trace::install_default_tracing();
        assert!(
            !second,
            "a global subscriber must exist after the first call"
        );
    }
}
