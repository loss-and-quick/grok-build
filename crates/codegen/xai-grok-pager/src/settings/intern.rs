//! Turning owned catalog data back into the `&'static` shapes the registry is
//! made of.
//!
//! [`SettingMeta`](super::SettingMeta) is `&'static str` throughout: it was a
//! compile-time table, and widening every field to `String` to accommodate rows
//! that arrive at runtime would cost the render path an allocation per row per
//! frame for the benefit of the handful of rows that need it.
//!
//! Rows now arrive at runtime in two ways — from a plugin manifest, and from
//! the wire catalog the whole registry is built from — so the pool below is on
//! the hot path of [`SettingsRegistry::defaults`](super::SettingsRegistry::defaults)
//! rather than of an occasional plugin reload. That is why the slices are
//! interned too, and not simply leaked: the same catalog raised a second time
//! must cost nothing, or every test that builds a registry would leak a fresh
//! copy of all fifty rows.

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

use super::registry::EnumChoice;

/// Intern a runtime string as `&'static str`.
pub fn intern(s: &str) -> &'static str {
    static POOL: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
    let pool = POOL.get_or_init(|| Mutex::new(HashSet::new()));
    let mut pool = pool.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(existing) = pool.get(s) {
        return existing;
    }
    let leaked: &'static str = Box::leak(s.to_string().into_boxed_str());
    pool.insert(leaked);
    leaked
}

/// Intern a list of already-interned strings as one `&'static` slice.
///
/// Used for `keywords` and for a group row's `children`.
pub fn intern_strs(items: &[&'static str]) -> &'static [&'static str] {
    static POOL: OnceLock<Mutex<HashSet<&'static [&'static str]>>> = OnceLock::new();
    let pool = POOL.get_or_init(|| Mutex::new(HashSet::new()));
    let mut pool = pool.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(existing) = pool.get(items) {
        return existing;
    }
    let leaked: &'static [&'static str] = Box::leak(items.to_vec().into_boxed_slice());
    pool.insert(leaked);
    leaked
}

/// Intern an enum row's choice catalog as one `&'static` slice.
pub fn intern_choices(items: &[EnumChoice]) -> &'static [EnumChoice] {
    static POOL: OnceLock<Mutex<HashSet<&'static [EnumChoice]>>> = OnceLock::new();
    let pool = POOL.get_or_init(|| Mutex::new(HashSet::new()));
    let mut pool = pool.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(existing) = pool.get(items) {
        return existing;
    }
    let leaked: &'static [EnumChoice] = Box::leak(items.to_vec().into_boxed_slice());
    pool.insert(leaked);
    leaked
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_content_interns_to_the_same_address() {
        let first = intern(&format!("theme{}", ""));
        let second = intern("theme");
        assert!(std::ptr::eq(first, second));

        let a = intern_strs(&[intern("scroll"), intern("wheel")]);
        let b = intern_strs(&[intern("scroll"), intern("wheel")]);
        assert!(std::ptr::eq(a, b));

        let choice = EnumChoice {
            canonical: intern("auto"),
            display: intern("Auto"),
            description: intern(""),
        };
        assert!(std::ptr::eq(
            intern_choices(&[choice]),
            intern_choices(&[choice])
        ));
    }

    #[test]
    fn different_content_interns_apart() {
        assert!(!std::ptr::eq(intern("hold"), intern("toggle")));
        assert!(!std::ptr::eq(
            intern_strs(&[intern("hold")]),
            intern_strs(&[intern("toggle")])
        ));
    }
}
