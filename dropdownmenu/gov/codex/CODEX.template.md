# Dropdownmenu Codex Template

Use this file and `../build-rules/BUILD-RULES.template.md` as the project instructions for the Rust dropdown-menu asset. Keep the implementation small and focused on reusable dropdown-menu behavior for desktop and web hosts. Keep host rendering and framework wiring in each host; expose framework-independent state, actions, transitions, and results from the asset.

## Folder ownership

- `gov/` contains all project paperwork: Codex and build rules, specifications, design notes, decisions, plans, and other governance or documentation files.
- `product/` contains product code only, including Rust manifests, source, tests, and product resources.
- `target/` contains Cargo build artifacts and disposable, one-time-use files only. It is generated output and may be deleted at any time; never put source or required project records here.

## Workflow

1. Read these instructions and the build rules before changing the asset.
2. Make the smallest complete product change that satisfies the operator request.
3. Keep product code in `product/` and generated or disposable output in `target/`.
4. Run only the focused checks required by the change and report their actual results.
5. Keep paths relative and preserve unrelated work.
