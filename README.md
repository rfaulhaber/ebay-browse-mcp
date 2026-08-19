# nix-rust-template

My personal project template for Rust projects using Nix.

## Creating a new project

The flake reads the package name and version from `Cargo.toml`, so the Nix
package, the `default` output, and the built binary all follow it. To rebrand a
fresh clone:

1. Set `name` and `version` in `Cargo.toml`.
2. Update the import in `src/main.rs` to match — Cargo maps the package name to
   a Rust identifier by replacing `-` with `_` (e.g. `my-project` →
   `use my_project::...`).
3. Edit this README's title and description.

## Setup

A minimal `.envrc` (`use flake`) is committed, so after cloning just allow it:

``` nu
direnv allow
```

For machine-specific tweaks (extra env vars, a different toolchain), create an
`.envrc.local` — it is gitignored and sourced automatically by `.envrc`.

