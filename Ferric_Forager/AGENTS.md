# Ferric Forager bootstrap

- [FF-BOOT-001] Read `START_HERE.yaml` completely before acting in this repository.
- [FF-BOOT-002] Read `.GOV/codex.yaml` completely before acting in this repository.
- [FF-BOOT-003] All disposable Cargo builds, tests, benchmarks, runtime-proof stages, command captures, temporary repositories, and validation scratch output must live under the ignored repository-root `.fforager-artifacts/` directory; do not create them in external temp roots, root `target/`, or `build/target/`.
- [FF-BOOT-004] Before selecting, starting, or moving to the next task, run `powershell -NoProfile -File build/scripts/clean-artifacts.ps1` and then its `-VerifyOnly` mode; do not advance until `.fforager-artifacts/` is verified empty.
- [FF-BOOT-005] Create every linked worktree under the ignored repository-root `.worktrees/` directory through `build/scripts/new-worktree.ps1`; the helper must link that worktree's `.fforager-artifacts` path to the primary repository's single artifact root, and external ad hoc worktree paths are forbidden.
- [FF-BOOT-006] Implementation is not complete until its closure commit is merged into canonical `main` and main containment is proven; shipped implementation must reside under `product/`, while prerequisite-only code without product-promotion approval remains decision-required.
- [FF-BOOT-007] At the start of every session, run `ferforstart.cmd`, read every injected `START_HERE.yaml`, `.GOV/codex.yaml`, and `.GOV/topology.yaml` authority file completely, acknowledge them as repository rules and instructions, and follow them and their authority precedence before acting.
