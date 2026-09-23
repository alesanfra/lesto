# Vendored: garde_derive 0.23.0

The files in this directory come from [garde_derive](https://github.com/jprochazk/garde) 0.23.0,
the derive macro of the garde validation library, dual-licensed MIT OR Apache-2.0. lesto uses
them under the Apache License 2.0 (see `LICENSE` at the repository root).

## Why

`garde_derive` hard-codes `::garde::` in the code it generates and has no `crate = ".."` option,
so every application deriving `Validate` needed garde as a direct dependency. `#[lesto::model]`
derives it through this copy instead, pointed at `::lesto::garde`, which makes the application's
manifest one line. The upstream derive is still what `#[derive(garde::Validate)]` uses.

## Changes from upstream

- Generated paths: `::garde::` → `::lesto::garde::`.
- Module paths: `crate::` → `crate::garde::`.
- `lib.rs` became `mod.rs`: only the `Validate` derive is kept, as a function taking a parsed
  `DeriveInput` (not a `#[proc_macro_derive]`); the `select!` macro is dropped.
- Cargo features: `email`, `url` and `regex` are features of `lesto-macros` with the same
  meaning, turned on by lesto's features of the same name. `credit-card`, `phone-number` and
  `js-sys` are never on, so those rules report that their feature is missing.

## Updating

When garde moves to a new version: copy `src/{check,emit,model,syntax,util}.rs` of the matching
`garde_derive` release over these files, re-apply the two path substitutions, and run
`cargo test --workspace` (the `model_*` tests in `crates/lesto/tests/integration.rs` exercise
the derive). The version in `[workspace.dependencies]` and the one vendored here must match: the
generated code calls into garde's runtime API.
