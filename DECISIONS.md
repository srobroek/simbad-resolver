# Decisions, Ambiguities & Inputs Needed

A running log of autonomous decisions, open ambiguities, and anything that needs
the maintainer's input. Newest at the top of each section.

> **Update 2026-07-12 — architecture superseded.** The project collapsed from the
> 8-crate workspace to a **single `simbad-resolver` crate** with a single **`redb`**
> storage backend (dropping `sqlx` + `dashmap`); see
> [ADR-0002](docs/adr/0002-single-crate.md). Entries below that describe the
> multi-crate split, the SQLite/`sqlx` durable backend, or `simbad-resolver-*`
> sub-crate paths are **historical**. `0.1.0` and `0.1.1` are published to
> crates.io (single crate); the five published sub-crates are yanked.

## Inputs needed from the maintainer

- ~~**crates.io publish / naming**~~ — **RESOLVED (2026-07-12)**: `simbad-resolver`
  `0.1.0`/`0.1.1` are published via crates.io Trusted Publishing (OIDC); the five
  published `simbad-resolver-*` sub-crates are yanked.
- **`astro-angle` / `target-match`**: both are greenfield siblings. This crate
  uses a decimal-degree seam now and will adopt `astro_angle::Equatorial` once
  that crate exists. No blocking dependency.
- **Dual-licensing**: currently Apache-2.0 only. Rust convention is dual
  `MIT OR Apache-2.0`; say the word and I'll add the MIT file + update metadata.

## Open ambiguities (assumed a default; flag if wrong)

- **MSRV** assumed `1.82`. Adjust if you target older.
- **Cone-search radius units**: degrees (matches ICRS decimal-degree convention).
- **Sesame XML format — live-verified 2026-07-13**: the Sesame resolver
  (`src/sesame/`) parses a **hand-built** `-oxp` (`SNV`) fixture based on the
  documented schema, NOT a byte-for-byte live capture. Endpoint
  `https://cds.unistra.fr/cgi-bin/nph-sesame/-oxp/SNV?<name>`. `just test-live`
  (`cargo test --test live -- --ignored`, covering both Sesame and TAP) passed
  against the live endpoints on 2026-07-13, including `sesame_resolves_m31`.
  The parser is tolerant (falls back to scanning the whole doc), but CDS can
  change the response shape without notice; re-run the ignored live tests
  periodically (the scheduled `live-simbad` CI workflow does this for TAP) to
  catch drift.
- **Sesame `common_name`** is always `None` (Sesame has no SIMBAD-`NAME` curated
  analog); enrichment via a supplied TAP resolver fills type/aliases when wanted.

## Autonomous decisions

- **`galdim_majaxis`/`galdim_minaxis`/`galdim_angle` cache-compat via
  `#[serde(default)]`, not a schema-version bump.** Added SIMBAD's angular-size
  columns (`ResolvedIdentity`/`CachedTarget` gain
  `galdim_majaxis_arcmin: Option<f64>`, `galdim_minaxis_arcmin: Option<f64>`,
  `galdim_angle_deg: Option<i16>`, units unconverted). The requesting task
  assumed an explicit cache schema-version field; the crate has no such
  mechanism — it already relies on per-field `#[serde(default)]` on
  `StoredTarget` for forward-compat (`v_mag` set this precedent). Reused that
  existing convention instead of introducing new versioning infrastructure;
  proven by `galdim_pre_upgrade_row_deserializes_with_none` in
  `src/cache/redb.rs`, which writes a pre-upgrade-shaped JSON blob directly into
  the `targets` table and reads it back through the current `Cache::get_by_id`.
- **Deviated from the full SpecKit implementation DAG** (tasks → checklist →
  analyze → agent-assign → verify → review → …) in favour of a direct,
  test-driven crate-by-crate build, because the goal is "take to completion,
  fully tested." `spec.md`/`plan.md`/`research.md`/`data-model.md`/`contracts/`
  were authored through SpecKit; `tasks.md` is generated for the record. Post-hoc
  quality gates (clippy/fmt/test/doc) are enforced via CI + `just lint`.
- **8-crate granular split** under `simbad-resolver-*` (see ADR-0001 / plan.md).
  *(Superseded → single crate; ADR-0002.)*
- **Async `Resolver`/`Cache`/`Queue` traits** (dyn-compatible via `async-trait`).
- **Settings are caller-owned config** — no persisted `resolver_settings` table.
- **Sesame standalone with optional enrichment** — no hard dep on `-tap`.
- **Durable backend = SQLite (`sqlx`)**; `redb` noted as a future pure-Rust option.
  *(Superseded → `redb` is now the sole storage backend; ADR-0002.)*
- **Dependencies unconstrained** (per maintainer): dropped astro-plan's hand-rolled
  ADQL percent-encoding, https endpoint check, `domain_core` dep, and dead `strsim`.
- **Configurable id namespace** (UUIDv5) replacing the hardcoded `astro-plan.targets`.
- **`otype_raw` escape hatch** retained alongside the closed `ObjectType`.
- **Caldwell translation lives in the facade** (so both TAP and Sesame benefit);
  direct `-tap`/`-sesame` users call `simbad-resolver-caldwell` themselves.
- **Batch `drain()` is sequential** within a pass (polite to CDS; each pending
  item processed at most once per call, transient failures released for retry).
  True bounded parallelism is a future enhancement (kept simple + correct for v1).
- **`apply_override`** makes the whole target `user-override` (sticky) and binds
  the supplied alias; returns `None` if the target id is unknown.
- **Parallel build via worktree subagents** (core/cache built directly; the 5
  leaf crates built in isolated worktrees and merged). One worktree branched
  from a pre-scaffold commit and self-corrected via `--ff-only`; no work lost.
