# Changelog

## [0.5.0](https://github.com/nightwatch-astro/simbad-resolver/compare/simbad-resolver-v0.4.0...simbad-resolver-v0.5.0) (2026-07-24)


### ⚠ BREAKING CHANGES

* **deps:** migrate to reqwest 0.13 ([#41](https://github.com/nightwatch-astro/simbad-resolver/issues/41))

### Bug Fixes

* **deps:** migrate to reqwest 0.13 ([#41](https://github.com/nightwatch-astro/simbad-resolver/issues/41)) ([1984706](https://github.com/nightwatch-astro/simbad-resolver/commit/1984706ad7ec39ff4761bdb9ca37ea996545e378))

## [0.4.0](https://github.com/nightwatch-astro/simbad-resolver/compare/simbad-resolver-v0.3.5...simbad-resolver-v0.4.0) (2026-07-22)


### ⚠ BREAKING CHANGES

* ResolvedIdentity::position(), CachedTarget::position(), and PositionMatch::position() now return skymath 0.6.0 types. Downstream crates with skymath 0.5 annotations or bounds must update their direct dependency to 0.6.0.

### Bug Fixes

* migrate public coordinate types to skymath 0.6.0 ([b409b32](https://github.com/nightwatch-astro/simbad-resolver/commit/b409b3237a6c3b7c86eb4866d7499ce8d0d7576d))

## [0.3.5](https://github.com/nightwatch-astro/simbad-resolver/compare/simbad-resolver-v0.3.4...simbad-resolver-v0.3.5) (2026-07-21)


### Bug Fixes

* **deps:** bump skymath to 0.5 ([#37](https://github.com/nightwatch-astro/simbad-resolver/issues/37)) ([9de64c7](https://github.com/nightwatch-astro/simbad-resolver/commit/9de64c7c3f654fe37b046f9ea942cc673ef7cd77))

## [0.3.4](https://github.com/nightwatch-astro/simbad-resolver/compare/simbad-resolver-v0.3.3...simbad-resolver-v0.3.4) (2026-07-17)


### Bug Fixes

* **deps:** pick up skymath 0.4 parse_date_obs trailing-Z fix ([#34](https://github.com/nightwatch-astro/simbad-resolver/issues/34)) ([d8fec03](https://github.com/nightwatch-astro/simbad-resolver/commit/d8fec039a1398649c4466e49c0c8b82682b5df3b))

## [0.3.3](https://github.com/nightwatch-astro/simbad-resolver/compare/simbad-resolver-v0.3.2...simbad-resolver-v0.3.3) (2026-07-17)


### Bug Fixes

* store CLA signatures on unprotected branch, allowlist owner ([#30](https://github.com/nightwatch-astro/simbad-resolver/issues/30)) ([da6c996](https://github.com/nightwatch-astro/simbad-resolver/commit/da6c99625624e4db357cef730f0ced4638014754))

## [0.3.2](https://github.com/nightwatch-astro/simbad-resolver/compare/simbad-resolver-v0.3.1...simbad-resolver-v0.3.2) (2026-07-14)


### Features

* **cache:** opt-in Eventual durability for upsert_batch + explicit flush ([#27](https://github.com/nightwatch-astro/simbad-resolver/issues/27)) ([4295e66](https://github.com/nightwatch-astro/simbad-resolver/commit/4295e66381e94a19086dd3bde2daf4c271132178))

## [0.3.1](https://github.com/nightwatch-astro/simbad-resolver/compare/simbad-resolver-v0.3.0...simbad-resolver-v0.3.1) (2026-07-14)


### Bug Fixes

* **cache:** make upsert_batch's dedup O(n) instead of O(n²) ([#25](https://github.com/nightwatch-astro/simbad-resolver/issues/25)) ([3a8af16](https://github.com/nightwatch-astro/simbad-resolver/commit/3a8af16e852daab39b22b78072eede09ea4563cf))

## [0.3.0](https://github.com/nightwatch-astro/simbad-resolver/compare/simbad-resolver-v0.2.3...simbad-resolver-v0.3.0) (2026-07-14)


### ⚠ BREAKING CHANGES

* relicense from Apache-2.0 to MPL-2.0 ([#21](https://github.com/nightwatch-astro/simbad-resolver/issues/21))

### Features

* **cache:** add Cache::upsert_batch, one write transaction per batch ([#24](https://github.com/nightwatch-astro/simbad-resolver/issues/24)) ([40c401c](https://github.com/nightwatch-astro/simbad-resolver/commit/40c401c3de68b74148fde95a1628b3bb4499c916))


### Bug Fixes

* use GitHub App token for CLA bot instead of PAT ([#23](https://github.com/nightwatch-astro/simbad-resolver/issues/23)) ([392208d](https://github.com/nightwatch-astro/simbad-resolver/commit/392208dff2aec87890a89c8712affea177ec17c5))


### Miscellaneous Chores

* relicense from Apache-2.0 to MPL-2.0 ([#21](https://github.com/nightwatch-astro/simbad-resolver/issues/21)) ([74ac44a](https://github.com/nightwatch-astro/simbad-resolver/commit/74ac44adb463a7ce5d686ed5c18af6a0ac912c9a))

## [0.2.3](https://github.com/nightwatch-astro/simbad-resolver/compare/simbad-resolver-v0.2.2...simbad-resolver-v0.2.3) (2026-07-13)


### Documentation

* compile the coordinates example so docs.rs no longer marks it untested ([#19](https://github.com/nightwatch-astro/simbad-resolver/issues/19)) ([dbd64b6](https://github.com/nightwatch-astro/simbad-resolver/commit/dbd64b6d9e01b1543905b2b333979f82cb303039))

## [0.2.2](https://github.com/nightwatch-astro/simbad-resolver/compare/simbad-resolver-v0.2.1...simbad-resolver-v0.2.2) (2026-07-13)


### Documentation

* render README + guide on docs.rs, add per-method examples ([#17](https://github.com/nightwatch-astro/simbad-resolver/issues/17)) ([302f9cc](https://github.com/nightwatch-astro/simbad-resolver/commit/302f9ccd58d5eebcc56d82aedae4dd4cf203db7f))

## [0.2.1](https://github.com/nightwatch-astro/simbad-resolver/compare/simbad-resolver-v0.2.0...simbad-resolver-v0.2.1) (2026-07-13)


### Documentation

* rewrite anthropomorphic error contracts, cover all capabilities, add guide ([#15](https://github.com/nightwatch-astro/simbad-resolver/issues/15)) ([f65b494](https://github.com/nightwatch-astro/simbad-resolver/commit/f65b494dc1919fcf94399b113d455760b6d3a58e))

## [0.2.0](https://github.com/nightwatch-astro/simbad-resolver/compare/simbad-resolver-v0.1.3...simbad-resolver-v0.2.0) (2026-07-12)


### ⚠ BREAKING CHANGES

* `SimbadResolver::new` takes a `CacheBackend` and returns `Result`; `SimbadResolver` and `BatchResolver` drop their cache/queue type parameters.
`ResolvedIdentity` and `CachedTarget` gain a `v_mag` field.

### Features

* select cache backend via CacheBackend; type-erase the facade ([#11](https://github.com/nightwatch-astro/simbad-resolver/issues/11)) ([eed8d31](https://github.com/nightwatch-astro/simbad-resolver/commit/eed8d31ee635014604afd0c6e07cd67c91ad9cb8))

## [0.1.3](https://github.com/nightwatch-astro/simbad-resolver/compare/simbad-resolver-v0.1.2...simbad-resolver-v0.1.3) (2026-07-12)


### Features

* typed skymath Equatorial position() on resolved-object types ([#9](https://github.com/nightwatch-astro/simbad-resolver/issues/9)) ([46750c8](https://github.com/nightwatch-astro/simbad-resolver/commit/46750c8038939c8ac2ce6f17f1c4455115f89946))

## [0.1.2](https://github.com/nightwatch-astro/simbad-resolver/compare/simbad-resolver-v0.1.1...simbad-resolver-v0.1.2) (2026-07-12)


### Features

* opt-in fuzzy (token-set) typeahead in search() ([#7](https://github.com/nightwatch-astro/simbad-resolver/issues/7)) ([7c316a8](https://github.com/nightwatch-astro/simbad-resolver/commit/7c316a8ca947b284cda38cdca8c97c1ad211da3d))

## [0.1.1](https://github.com/nightwatch-astro/simbad-resolver/compare/simbad-resolver-v0.1.0...simbad-resolver-v0.1.1) (2026-07-12)


### Features

* **cache-memory:** in-memory Cache + Queue (dashmap) ([4de52ae](https://github.com/nightwatch-astro/simbad-resolver/commit/4de52ae84576d1c25397eaf87b7ff5b90d6d3708))
* **cache-sqlite:** durable Cache + cache-backed Queue (sqlx + migrations) ([b61957e](https://github.com/nightwatch-astro/simbad-resolver/commit/b61957ead6533bb6e957db19136ed9f9fbd4dfa3))
* **cache:** pluggable Cache + Queue traits and read models ([df9eed3](https://github.com/nightwatch-astro/simbad-resolver/commit/df9eed393ae893593ef896dd519f5602fa42ceb6))
* **caldwell:** C1-C109 designation map + parse_caldwell_number ([345df05](https://github.com/nightwatch-astro/simbad-resolver/commit/345df05f61f8fab38f836d7753eb872660ce4542))
* **core:** pure types, normalize, identity, Resolver trait, wire helpers ([0256834](https://github.com/nightwatch-astro/simbad-resolver/commit/0256834987ffce8c71466ed4aa300a214b74db96))
* **facade:** orchestration (cache-first resolve, sticky override, batch) ([56df7d5](https://github.com/nightwatch-astro/simbad-resolver/commit/56df7d51f3ce56c723ce334fdd4437ad25165c0b))
* scaffold 8-crate workspace skeleton + decisions log ([932058e](https://github.com/nightwatch-astro/simbad-resolver/commit/932058e56c6acf4a3be88438bd17357c3bb12f43))
* **sesame:** SIMBAD Sesame resolver with optional enrichment ([41191d4](https://github.com/nightwatch-astro/simbad-resolver/commit/41191d4e6a31da8e88b98b18c9b90ab43dfae63c))
* **tap:** SIMBAD TAP resolver (name resolve + cone search) ([6463cba](https://github.com/nightwatch-astro/simbad-resolver/commit/6463cba04cec4fa78566e7ed225f6bedb281012b))
