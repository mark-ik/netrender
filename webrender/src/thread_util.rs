/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Thread-spawning helpers that dispatch to `wasm_thread` on wasm32 targets
//! (when the `wasm` feature is enabled) and to `std::thread` otherwise.
//!
//! ## Wasm usage
//!
//! When the `wasm` feature is enabled, the hosting page must:
//! 1. Serve with cross-origin isolation headers:
//!    - `Cross-Origin-Opener-Policy: same-origin`
//!    - `Cross-Origin-Embedder-Policy: require-corp`
//! 2. Call `wasm_bindgen_rayon::init_thread_pool(num_threads)` from JS
//!    before creating a WebRender instance.

use std::io;

/// Spawn a named background thread.
///
/// On native targets this uses `std::thread::Builder` with a thread name.
/// On wasm32 with the `wasm` feature, this uses `wasm_thread::spawn` which
/// creates a Web Worker backed thread (requires SharedArrayBuffer).
#[cfg(not(feature = "wasm"))]
pub fn spawn_named<F>(name: String, f: F) -> io::Result<()>
where
    F: FnOnce() + Send + 'static,
{
    std::thread::Builder::new().name(name).spawn(f)?;
    Ok(())
}

#[cfg(feature = "wasm")]
pub fn spawn_named<F>(_name: String, f: F) -> io::Result<()>
where
    F: FnOnce() + Send + 'static,
{
    wasm_thread::spawn(f);
    Ok(())
}

/// Re-export `wasm-bindgen-rayon`'s thread pool initializer so the wasm
/// embedder can call it.  On native, this function does not exist — rayon
/// manages its own pool.
#[cfg(feature = "wasm")]
pub use wasm_bindgen_rayon::init_thread_pool as init_wasm_rayon_pool;
