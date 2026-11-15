# Changelog


## [1.1.3](https://github.com/runningman84/gh-token/compare/v1.1.2...v1.1.3) (2026-11-15)


### Bug Fixes

* cilium policy hostname ([2d0aaee](https://github.com/runningman84/gh-token/commit/2d0aaee85406a63affed7a1c1ab8c869ef348464))
* use correct tag ([bb9c0a0](https://github.com/runningman84/gh-token/commit/bb9c0a05b888aa99a7fe6d1e68be5a2d2482029f))

## [1.1.2](https://github.com/runningman84/gh-token/compare/v1.1.1...v1.1.2) (2026-11-15)


### Bug Fixes

* upload SBOM and scan artifacts to the release ([93666e0](https://github.com/runningman84/gh-token/commit/93666e083d64793f24da4eff089d597db20d4742))

## [1.1.1](https://github.com/runningman84/gh-token/compare/v1.1.0...v1.1.1) (2026-11-15)


### Bug Fixes

* **chart:** add placeholder for GH_APP_KEY_B64 to satisfy schema validation ([c8ec9df](https://github.com/runningman84/gh-token/commit/c8ec9df2cd15f4bd5fd76ca8e54398ab42b9008e))
* correct the helm chart OCI path and use native syft/grype ([33df46d](https://github.com/runningman84/gh-token/commit/33df46db3e009c4e0ee960044fddfe9bf09193c3)), closes [charts/gh-token/#token](https://github.com/charts/gh-token//issues/token)

# [1.1.0](https://github.com/runningman84/gh-token/compare/v1.0.3...v1.1.0) (2026-11-15)


### Bug Fixes

* bump version files before the release commit ([8c0a8cb](https://github.com/runningman84/gh-token/commit/8c0a8cb0847533b13a749417386d58de398c5bf5))


### Features

* add --version/--help to the server and log the version at startup ([8d66c29](https://github.com/runningman84/gh-token/commit/8d66c29f230b663a4426ac39698904bb528c6dcd))

## [1.0.3](https://github.com/runningman84/gh-token/compare/v1.0.2...v1.0.3) (2026-11-15)


### Bug Fixes

* repair image and helm-chart publishing steps ([f89b955](https://github.com/runningman84/gh-token/commit/f89b9553fc7243ac7e19b7f36b09bec2d556162f))

## [1.0.2](https://github.com/runningman84/gh-token/compare/v1.0.1...v1.0.2) (2026-11-15)


### Bug Fixes

* package release archives into dist/ and trim the build matrix ([1797a39](https://github.com/runningman84/gh-token/commit/1797a3995195c3c4895c87f1a9d402047f5d4ddf))

## [1.0.1](https://github.com/runningman84/gh-token/compare/v1.0.0...v1.0.1) (2026-11-15)


### Bug Fixes

* build release binaries natively instead of via a missing action ([54f97cf](https://github.com/runningman84/gh-token/commit/54f97cf213e8bb6ff004e8496d38c2e84e700818))

# 1.0.0 (2026-11-15)


### Bug Fixes

* remove runs-on from reusable workflow call in publish job ([c0f6a01](https://github.com/runningman84/gh-token/commit/c0f6a01bc47236b0862ab06036a34013fb70b629))


### Features

* add gh-token server, client, helm chart and release pipeline ([9d0e04f](https://github.com/runningman84/gh-token/commit/9d0e04fb3053d120913a92f04658be40fbc7ff04)), closes [#token](https://github.com/runningman84/gh-token/issues/token)
* expose ghts_build_info metric with the server version ([87f38b5](https://github.com/runningman84/gh-token/commit/87f38b539c1a2f66c2423820f0e12d0b93fb3b71))
* expose standard process_* metrics on /metrics ([212574e](https://github.com/runningman84/gh-token/commit/212574e939efb46289e26ebf6a27fe0396ee9b9c))
* token hardening, purpose metadata, and per-client keys ([83803b4](https://github.com/runningman84/gh-token/commit/83803b485f2e7815e1c4909efadb4fb9c2fb5f88)), closes [X-#Token-Key](https://github.com/X-/issues/Token-Key) [#token](https://github.com/runningman84/gh-token/issues/token)

# Changelog
