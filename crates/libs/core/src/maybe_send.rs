//! Conditional `Send`/`Sync` bounds for multi-runtime compatibility.
//!
//! On native targets (x86_64, aarch64, etc.), `MaybeSend` resolves to `Send`
//! and `MaybeSync` resolves to `Sync`. This satisfies Tokio's requirement
//! that spawned futures are `Send`.
//!
//! On `wasm32` targets, these traits are no-ops — blanket-implemented for all
//! types. This allows Cloudflare Workers code to use `!Send` JS interop types
//! (`Rc<RefCell<...>>`, `JsValue`, etc.) without constraint violations.
//!
//! ## Usage
//!
//! Use `MaybeSend` instead of `Send` in trait bounds throughout the core:
//!
//! ```rust,ignore
//! use source_coop_core::maybe_send::MaybeSend;
//!
//! pub trait MyTrait: MaybeSend {
//!     fn do_work(&self) -> impl Future<Output = ()> + MaybeSend;
//! }
//! ```

// --- Native targets: MaybeSend = Send, MaybeSync = Sync ---

#[cfg(not(target_arch = "wasm32"))]
pub trait MaybeSend: Send {}
#[cfg(not(target_arch = "wasm32"))]
impl<T: Send> MaybeSend for T {}

#[cfg(not(target_arch = "wasm32"))]
pub trait MaybeSync: Sync {}
#[cfg(not(target_arch = "wasm32"))]
impl<T: Sync> MaybeSync for T {}

// --- WASM targets: MaybeSend and MaybeSync are no-ops ---

#[cfg(target_arch = "wasm32")]
pub trait MaybeSend {}
#[cfg(target_arch = "wasm32")]
impl<T> MaybeSend for T {}

#[cfg(target_arch = "wasm32")]
pub trait MaybeSync {}
#[cfg(target_arch = "wasm32")]
impl<T> MaybeSync for T {}
