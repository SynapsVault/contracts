## Summary

<!-- What does this change and why? -->

## Checklist

- [ ] `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` pass
- [ ] New behaviour is covered by tests (including the unauthorised / error path)
- [ ] `docs/CONTRACTS.md` and rustdoc updated for any interface, storage, event or error change
- [ ] Error codes were appended, not renumbered
