//! Test-support stub for the Latchkey workspace (owned by LATCH-2 / F02).
//!
//! This package exists so contract tests can make **compile-time** assertions
//! that types do *not* implement a trait — something stable Rust cannot
//! express directly (there are no negative trait bounds). It is a
//! dev-dependency of the `latchkey` package only: the production binary and
//! the release/OCI gates never build or link it.
//!
//! # The `assert_not_serialize!` probe
//!
//! `assert_not_serialize!(SomeType)` compiles if and only if `SomeType` does
//! **not** implement `serde::Serialize`, and it additionally asserts (at
//! runtime) that the "not serializable" branch was the one that resolved.
//!
//! How it works: two traits each provide a method named `verdict` for
//! `Probe<T>` — one impl is bounded by `T: serde::Serialize`, the other is
//! unbounded. Method resolution at the expansion site (where `T` is a
//! concrete type) then behaves as follows:
//!
//! * if `T: Serialize`, both traits are in scope and both impls apply, so
//!   the call `probe.verdict()` is **ambiguous** and rustc fails the build
//!   with `E0034` (multiple applicable items in scope);
//! * if `T: !Serialize`, only the unbounded impl applies, the call
//!   resolves, and the returned verdict is `"no-serde-serialize"`, which
//!   the macro asserts.
//!
//! The assertion is therefore checked at compile time (the ambiguity is a
//! hard error) and re-confirmed at runtime (the resolved verdict), which is
//! exactly what the contracts suite needs for `SensitiveHeaders` and the
//! types that contain it.

use core::marker::PhantomData;

/// Marker used by [`assert_not_serialize!`]; construct it with [`Probe::new`].
///
/// The `fn() -> T` phantom keeps `Probe<T>` `Send + Sync` regardless of `T`
/// so the probe can be used from async tests. `Default` is derived purely
/// to satisfy `clippy::new_without_default`; tests construct via `new()`.
#[derive(Default)]
pub struct Probe<T>(PhantomData<fn() -> T>);

impl<T> Probe<T> {
    /// Create a probe marker for `T`.
    pub const fn new() -> Self {
        Self(PhantomData)
    }
}

/// Candidate verdict for types that **do** implement [`serde::Serialize`].
///
/// Only implemented when `T: serde::Serialize`; see the module docs for how
/// the method-name ambiguity turns that bound into a compile error.
pub trait SerializeImplemented {
    /// Verdict string returned when the type implements `Serialize`.
    fn verdict(&self) -> &'static str {
        "implements-serde-serialize"
    }
}

impl<T: serde::Serialize> SerializeImplemented for Probe<T> {}

/// Candidate verdict for types that may or may not implement
/// [`serde::Serialize`]; unbounded, so it always exists.
pub trait SerializeAbsent {
    /// Verdict string returned when only this impl resolves.
    fn verdict(&self) -> &'static str {
        "no-serde-serialize"
    }
}

impl<T> SerializeAbsent for Probe<T> {}

/// Assert at compile time (and re-check at runtime) that `$ty` does **not**
/// implement `serde::Serialize`.
///
/// Expanding this macro for a type that *does* implement `Serialize` is a
/// hard compile error (ambiguous `verdict` method call). See the module
/// documentation for the full mechanism.
///
/// # Examples
///
/// ```ignore
/// latchkey_test_support::assert_not_serialize!(my_crate::SensitiveHeaders);
/// ```
#[macro_export]
macro_rules! assert_not_serialize {
    ($ty:ty) => {{
        // Both traits must be in scope for the ambiguity to trigger when
        // `$ty: Serialize`; each is individually used for resolution, so
        // silence the "unused import" case when only one impl applies.
        #[allow(unused_imports)]
        use $crate::{SerializeAbsent, SerializeImplemented};
        let probe = $crate::Probe::<$ty>::new();
        let verdict: &'static str = probe.verdict();
        assert_eq!(
            verdict, "no-serde-serialize",
            concat!(
                "expected `",
                stringify!($ty),
                "` to NOT implement serde::Serialize, but resolution picked the \
                 Serialize impl (the build should have failed with an ambiguous \
                 `verdict` call)"
            )
        );
    }};
}
