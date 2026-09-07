//! The build must enable exactly one `jsonwebtoken` crypto backend.
//!
//! `jsonwebtoken` picks its process-wide provider from its own crate features. With both
//! `rust_crypto` and `aws_lc_rs` enabled it cannot choose, and instead caches a provider whose
//! signer and verifier factories `panic!` — in a `OnceLock`, on the first JWT touched anywhere in
//! the process, permanently. Nothing fails to build and nothing warns; whichever code path signs
//! first decides, and every JWT after it panics. `install_default` then returns `Err` forever,
//! so a guard that runs second cannot undo it.
//!
//! Cargo unifies features across the whole graph, so a single transitive dependency asking for
//! the other backend is enough to bring the ambiguity back. This test lives alone in its own
//! binary and signs before anything can install a provider, so that regression surfaces here and
//! names its cause, rather than in whichever auth test the runner happens to schedule first.

/// Signing and verifying must both work with no provider installed, which is only true when the
/// crate features resolve to exactly one backend.
#[test]
fn crate_features_resolve_to_exactly_one_jwt_backend() {
    // Half of the assertion is the compiler's: this path exists only while `rust_crypto` is on,
    // and that is the backend the auth code and its tests are written against.
    let _pinned: &jsonwebtoken::crypto::CryptoProvider =
        &jsonwebtoken::crypto::rust_crypto::DEFAULT_PROVIDER;

    let encoding = jsonwebtoken::EncodingKey::from_secret(b"jwt-backend-pin");
    let decoding = jsonwebtoken::DecodingKey::from_secret(b"jwt-backend-pin");
    let header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::HS256);
    let claims = serde_json::json!({ "exp": 9_999_999_999u64 });

    let signed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        jsonwebtoken::encode(&header, &claims, &encoding).expect("HS256 signing")
    }));
    let token = signed.expect(
        "`jsonwebtoken` has no unambiguous backend, so it cached a panicking provider. \
         A dependency has re-enabled its second backend: check the `gcloud-storage` features in \
         the workspace `Cargo.toml` and `cargo tree -e features -i jsonwebtoken`.",
    );

    let mut validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::HS256);
    validation.validate_aud = false;
    let verified = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        jsonwebtoken::decode::<serde_json::Value>(&token, &decoding, &validation)
            .expect("HS256 verification")
    }));
    verified.expect("verifying a JWT must not depend on a provider having been installed first");
}
