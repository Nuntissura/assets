# Dropdownmenu Build Rules Template

- Keep reusable dropdown behavior independent of desktop and web GUI frameworks.
- Keep host rendering, styling, layout, and event wiring in host adapters.
- Keep product code, Rust manifests, tests, and required product resources in `product/` only.
- Keep all project paperwork and documentation in `gov/` only.
- Keep Cargo build output and disposable one-time-use files in `target/`; nothing in `target/` may be required to build or use the product.
- Prefer focused dependencies and portable relative paths.
- For behavior changes, add or update focused tests for the public behavior and run the relevant Rust checks.
- Do not add workflow machinery or unrelated infrastructure to this small asset.
