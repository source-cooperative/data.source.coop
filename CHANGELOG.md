# Changelog

## [2.3.0](https://github.com/source-cooperative/data.source.coop/compare/v2.2.2...v2.3.0) (2026-07-22)


### Features

* **backend:** enable GCS backend (multistore gcp + wasm-compatible signing) ([#191](https://github.com/source-cooperative/data.source.coop/issues/191)) ([33f2b25](https://github.com/source-cooperative/data.source.coop/commit/33f2b253e55c1973f5fd4cffad797edef8937146))


### Bug Fixes

* **gcs:** pass bucket_name to multistore GCS store ([#193](https://github.com/source-cooperative/data.source.coop/issues/193)) ([0efb66f](https://github.com/source-cooperative/data.source.coop/commit/0efb66fefbf0a88eccfdff212b0c54ed986f0ecc))

## [2.2.2](https://github.com/source-cooperative/data.source.coop/compare/v2.2.1...v2.2.2) (2026-07-14)


### Bug Fixes

* bump multistore to 0.6.4 (multipart SignatureDoesNotMatch on keys with `=`) ([#181](https://github.com/source-cooperative/data.source.coop/issues/181)) ([c08261d](https://github.com/source-cooperative/data.source.coop/commit/c08261d2e7c800af7069ac073c80dd65db2cf64d))
* **ci:** stub the Source API; hermetic PR gate with authenticated write, contract, and failure-mode tests ([#183](https://github.com/source-cooperative/data.source.coop/issues/183)) ([a811d78](https://github.com/source-cooperative/data.source.coop/commit/a811d78a9d2f159e85196cac3c4f49a1d76f6db9))

## [2.2.1](https://github.com/source-cooperative/data.source.coop/compare/v2.2.0...v2.2.1) (2026-07-03)


### Bug Fixes

* correct prod auth audiences ([5d216e6](https://github.com/source-cooperative/data.source.coop/commit/5d216e6eba0ca61116d2af243a94fe9b3a21d3bd))

## [2.2.0](https://github.com/source-cooperative/data.source.coop/compare/v2.1.2...v2.2.0) (2026-07-01)


### Features

* accept multiple audiences for /.sts token exchange ([#163](https://github.com/source-cooperative/data.source.coop/issues/163)) ([e911496](https://github.com/source-cooperative/data.source.coop/commit/e911496769d5eb043831ac8bcd7d5774005d0aa6))
* **analytics:** log request duration and client IP ([#153](https://github.com/source-cooperative/data.source.coop/issues/153)) ([cad41b1](https://github.com/source-cooperative/data.source.coop/commit/cad41b10b2055fd4848541eb7618ac4356b102ea))
* authorize and enable writes to data connections ([#162](https://github.com/source-cooperative/data.source.coop/issues/162)) ([85972e8](https://github.com/source-cooperative/data.source.coop/commit/85972e89e270184f6ba64bb5d14006bae8053494))
* make STS max session TTL configurable via env var ([#165](https://github.com/source-cooperative/data.source.coop/issues/165)) ([39d15f5](https://github.com/source-cooperative/data.source.coop/commit/39d15f5d11423bbc033ac8b8588ecab6e4bb746a))
* OIDC provider ([#132](https://github.com/source-cooperative/data.source.coop/issues/132)) ([5671b64](https://github.com/source-cooperative/data.source.coop/commit/5671b64bcf104780d51376accf007580d2842e80))
* per-connection backend authentication via OIDC federation ([#147](https://github.com/source-cooperative/data.source.coop/issues/147)) ([2f7a12f](https://github.com/source-cooperative/data.source.coop/commit/2f7a12f807dee5cc971d0f6eaee99269074139d1))
* **worker:** aggregate live-globe activity by datacenter ([#171](https://github.com/source-cooperative/data.source.coop/issues/171)) ([c0a3169](https://github.com/source-cooperative/data.source.coop/commit/c0a31695ad0157b66aa283a5a0ca16c8b1bf920e))


### Bug Fixes

* **deps:** bump quinn-proto to 0.11.15 (RUSTSEC-2026-0185) ([#161](https://github.com/source-cooperative/data.source.coop/issues/161)) ([189e348](https://github.com/source-cooperative/data.source.coop/commit/189e348b78c0c63a74eefb5d925b1bbd4d16ad00))
* **registry:** sync product model with source.coop[#284](https://github.com/source-cooperative/data.source.coop/issues/284) (drop mirror config, use visibility) ([#149](https://github.com/source-cooperative/data.source.coop/issues/149)) ([8ecf9b4](https://github.com/source-cooperative/data.source.coop/commit/8ecf9b469b3cfe7b9d19533e1fbf758256cc4af5))
* return clear 400 for keyless writes instead of misleading sha256 error ([#168](https://github.com/source-cooperative/data.source.coop/issues/168)) ([f1187f5](https://github.com/source-cooperative/data.source.coop/commit/f1187f521567a60c22467d733fdf4428564ba057))
* **sigv4:** use encoded request path for inbound signature verification ([#176](https://github.com/source-cooperative/data.source.coop/issues/176)) ([56a9520](https://github.com/source-cooperative/data.source.coop/commit/56a9520006cd9ddc97c4b3cfcbde8fb4e796fef2))
* **sts:** bound the AssumeRoleWithWebIdentity call with a request timeout ([#172](https://github.com/source-cooperative/data.source.coop/issues/172)) ([fa463c7](https://github.com/source-cooperative/data.source.coop/commit/fa463c72e1387bfa0d671b766c06a5e74db91675))

## [2.1.2](https://github.com/source-cooperative/data.source.coop/compare/v2.1.1...v2.1.2) (2026-05-29)


### Miscellaneous Chores

* deploy observabilty changes ([9ea6b77](https://github.com/source-cooperative/data.source.coop/commit/9ea6b77c2bc115d29062b6b4038f254424ca98f7))

## [2.1.1](https://github.com/source-cooperative/data.source.coop/compare/v2.1.0...v2.1.1) (2026-04-02)


### Bug Fixes

* update multistore to properly recognize directory markers ([#125](https://github.com/source-cooperative/data.source.coop/issues/125)) ([25b1072](https://github.com/source-cooperative/data.source.coop/commit/25b1072ce97dc2d8880ac037b9d8796ebb0d3d8a))

## [2.1.0](https://github.com/source-cooperative/data.source.coop/compare/v2.0.0...v2.1.0) (2026-03-28)


### Features

* real-time public log stream via Durable Objects ([#122](https://github.com/source-cooperative/data.source.coop/issues/122)) ([3bf3524](https://github.com/source-cooperative/data.source.coop/commit/3bf3524a23e4b180946613f4c6ab6ff99dd2caf3))


### Bug Fixes

* **log-stream:** prevent external requests ([a8095f6](https://github.com/source-cooperative/data.source.coop/commit/a8095f6760584621a50ee578d30fe4448201af28))
* use staging log stream ([be7f97a](https://github.com/source-cooperative/data.source.coop/commit/be7f97a743731fbd5b05d3a75d756e0a7e53ccd7))

## [2.0.0](https://github.com/source-cooperative/data.source.coop/compare/v1.1.0...v2.0.0) (2026-03-26)


### ⚠ BREAKING CHANGES

* rebuild proxy with multistore and cloudflare workers runtime ([#116](https://github.com/source-cooperative/data.source.coop/issues/116))

### Features

* add Analytics Engine request logging ([#119](https://github.com/source-cooperative/data.source.coop/issues/119)) ([d9ab62f](https://github.com/source-cooperative/data.source.coop/commit/d9ab62f8add085feeaff723d79e820fa990b2284))
* rebuild proxy with multistore and cloudflare workers runtime ([#116](https://github.com/source-cooperative/data.source.coop/issues/116)) ([3e07478](https://github.com/source-cooperative/data.source.coop/commit/3e07478a1aa251ca2a7acee77810421a270069c7))


### Bug Fixes

* deserialize before caching ([295b965](https://github.com/source-cooperative/data.source.coop/commit/295b965972b51d8b14fd159736ac5472574bb776))
* Return 502 on API error instead of empty list ([d0ff1f0](https://github.com/source-cooperative/data.source.coop/commit/d0ff1f0f2cb53212879243740f1de07f79a4ac7f))
* scrutinise list type more closely ([d63e4f8](https://github.com/source-cooperative/data.source.coop/commit/d63e4f837134f8b24505c2dbde500538336fd892))

## [1.1.0](https://github.com/source-cooperative/data.source.coop/compare/v1.0.4...v1.1.0) (2026-03-05)


### Features

* Indicate range request support with Accept-Ranges header ([#104](https://github.com/source-cooperative/data.source.coop/issues/104)) ([4c6737a](https://github.com/source-cooperative/data.source.coop/commit/4c6737a8c58883e1f88262b77b53e24edd967d6f))


### Bug Fixes

* handle Range header in HEAD requests ([#114](https://github.com/source-cooperative/data.source.coop/issues/114)) ([24323a0](https://github.com/source-cooperative/data.source.coop/commit/24323a009409e1d769bccc9072329708da675db9))

## [1.0.4](https://github.com/source-cooperative/data.source.coop/compare/v1.0.3...v1.0.4) (2025-10-29)


### Bug Fixes

* update to no longer expect roles in product metadata ([8a8c630](https://github.com/source-cooperative/data.source.coop/commit/8a8c6300cadccbca57f401e3471915b413666f02))

## [1.0.3](https://github.com/source-cooperative/data.source.coop/compare/v1.0.2...v1.0.3) (2025-10-24)


### Performance Improvements

* persist source client across requests ([#95](https://github.com/source-cooperative/data.source.coop/issues/95)) ([453b2a8](https://github.com/source-cooperative/data.source.coop/commit/453b2a80ff22fc6953f879019aafa77163b4b2b8))

## [1.0.2](https://github.com/source-cooperative/data.source.coop/compare/v1.0.1...v1.0.2) (2025-10-09)


### Bug Fixes

* expose content-range header ([#93](https://github.com/source-cooperative/data.source.coop/issues/93)) ([f3fbc3d](https://github.com/source-cooperative/data.source.coop/commit/f3fbc3d650140d28a2c876c9e63fab6004c1c68b))

## [1.0.1](https://github.com/source-cooperative/data.source.coop/compare/v1.0.0...v1.0.1) (2025-09-30)


### Bug Fixes

* update source api types to match S2 codebase ([#90](https://github.com/source-cooperative/data.source.coop/issues/90)) ([6dec35d](https://github.com/source-cooperative/data.source.coop/commit/6dec35d60563943fac1d40ce48139859631e540f))

## [1.0.0](https://github.com/source-cooperative/data.source.coop/compare/v0.1.29...v1.0.0) (2025-08-21)


### ⚠ BREAKING CHANGES

* update to accomodate Product in s2 API

### Features

* add headers to requests to source API ([#81](https://github.com/source-cooperative/data.source.coop/issues/81)) ([edda62f](https://github.com/source-cooperative/data.source.coop/commit/edda62f37d7914cb76209fe7b7209a78e1e49b3c))
* update to accomodate Product in s2 API ([be44f43](https://github.com/source-cooperative/data.source.coop/commit/be44f43e5497fbb26d46d180162cfffb269dce1b))
* use Squid proxy for communication with Vercel API ([#85](https://github.com/source-cooperative/data.source.coop/issues/85)) ([25438c3](https://github.com/source-cooperative/data.source.coop/commit/25438c362e9cd1c7d52f5c4d2932542466eef01a))


### Bug Fixes

* don't specify accept-encoding, letting reqwest handle decompression automatically ([c914e77](https://github.com/source-cooperative/data.source.coop/commit/c914e77b8d0495ec229a2575f8aebbeb41947e8d))
* lowercase header names ([6b141fc](https://github.com/source-cooperative/data.source.coop/commit/6b141fc34da307593d4fa2526b19becf2f4e1a12))
* **model:** mv tags & roles to metadata ([571eb94](https://github.com/source-cooperative/data.source.coop/commit/571eb9476cc439b04cf0ecd959e8ca83b3107ccc))
* update data model to match API ([b6d4032](https://github.com/source-cooperative/data.source.coop/commit/b6d40327a2eda5ff8fb5f69bc7d9e7b67a5d04e2))
* update source api emails struct ([fbdd02a](https://github.com/source-cooperative/data.source.coop/commit/fbdd02a31627a0f3deb68dc7acd613b041c00bdb))


### Miscellaneous Chores

* fix release version ([a7cbe0f](https://github.com/source-cooperative/data.source.coop/commit/a7cbe0fbe222beca84db6d2e9ed98da2c9cda42c))
* fix release version ([e97c41f](https://github.com/source-cooperative/data.source.coop/commit/e97c41fff7b9f5cd5645c97da4ed2c7d2d143d65))

## [0.1.29](https://github.com/source-cooperative/data.source.coop/compare/v0.1.28...v0.1.29) (2025-05-29)


### Bug Fixes

* **errors:** pass through client error status codes ([#77](https://github.com/source-cooperative/data.source.coop/issues/77)) ([fe383dd](https://github.com/source-cooperative/data.source.coop/commit/fe383dd08f95d2b6109efa34521815990ece9e0b))
* **logging:** only log server errors, remove unnecessary details from logs ([#75](https://github.com/source-cooperative/data.source.coop/issues/75)) ([496373c](https://github.com/source-cooperative/data.source.coop/commit/496373c70e77f22f064182641c37ac0f1c6fbef7))

## [0.1.28](https://github.com/source-cooperative/data.source.coop/compare/v0.1.27...v0.1.28) (2025-05-28)


### Bug Fixes

* handle unknown Rusoto errors with 404 status ([#73](https://github.com/source-cooperative/data.source.coop/issues/73)) ([8375d50](https://github.com/source-cooperative/data.source.coop/commit/8375d5013a8e559cb2c365722859c70c709ebc68))

## [0.1.27](https://github.com/source-cooperative/data.source.coop/compare/v0.1.26...v0.1.27) (2025-05-27)


### Bug Fixes

* treat missing objects as 404s ([#69](https://github.com/source-cooperative/data.source.coop/issues/69)) ([8f4efbf](https://github.com/source-cooperative/data.source.coop/commit/8f4efbf897afaa354b5aab4d5393d69939249ab1))

## [0.1.26](https://github.com/source-cooperative/data.source.coop/compare/v0.1.25...v0.1.26) (2025-05-15)


### Bug Fixes

* Rectify crate version ([#61](https://github.com/source-cooperative/data.source.coop/issues/61)) ([00fa9db](https://github.com/source-cooperative/data.source.coop/commit/00fa9db0cc6dee84d7abbfcf9d633a41d1a24f2d))


### Refactor

* refactor: simplify repository fetching logic in SourceAPI ([#62](https://github.com/source-cooperative/data.source.coop/issues/62)) ([c739a3a](https://github.com/source-cooperative/data.source.coop/commit/c739a3ad2501ac5c8e0bf9a8f6ccf4c8632b7e61))

## [0.1.25](https://github.com/source-cooperative/data.source.coop/compare/v0.1.24...v0.1.25) (2025-05-13)


### Improvements

* Add more clarity to cloud provider errors ([#60](https://github.com/source-cooperative/data.source.coop/pull/60)) ([29837a3](https://github.com/source-cooperative/data.source.coop/commit/29837a357172161037a33ab0dad32c0ae3744007))


## [0.1.24](https://github.com/source-cooperative/data.source.coop/compare/v0.1.23...v0.1.24) (2025-05-15)


### Improvements

* Observability/convert error handling ([#59](https://github.com/source-cooperative/data.source.coop/pull/59)) ([562c2de](https://github.com/source-cooperative/data.source.coop/commit/562c2dea3b50c643b749d50a7419fdad991e9cd4))

## [0.1.23](https://github.com/source-cooperative/data.source.coop/compare/v0.1.22...v0.1.23) (2025-05-15)


### Improvements

* Log unexpected errors ([d339a01](https://github.com/source-cooperative/data.source.coop/commit/d339a01a43ce2fe01745dffa17e410ed5a156ec4))

## [0.1.22](https://github.com/source-cooperative/data.source.coop/compare/v0.1.21...v0.1.22) (2025-05-15)


### Bug Fixes

* File empty on mv ([#54](https://github.com/source-cooperative/data.source.coop/pull/54)) ([d4e329e](https://github.com/source-cooperative/data.source.coop/commit/d4e329e5424cd66ad7930a90685388385e684147))


### Improvements

* Updated release versions ([#56](https://github.com/source-cooperative/data.source.coop/pull/56)) ([c8b44b6](https://github.com/source-cooperative/data.source.coop/commit/c8b44b68b9b672beebc20324e2c63d34675ad48d))
* More targetted error handling ([#58](https://github.com/source-cooperative/data.source.coop/pull/58)) ([90e3475](https://github.com/source-cooperative/data.source.coop/commit/90e34750ceabe7281e3cc5dfb003982240e83217))

## [0.1.21](https://github.com/source-cooperative/data.source.coop/compare/v0.1.20...v0.1.21) (2025-03-11)


### Bug Fixes

* file empty on mv ([#51](https://github.com/source-cooperative/data.source.coop/issues/51)) ([1f1b3fa](https://github.com/source-cooperative/data.source.coop/commit/1f1b3fa24b175162965281a50c4f50592e1046f8))

## [0.1.20](https://github.com/source-cooperative/data.source.coop/compare/v0.1.19...v0.1.20) (2024-12-03)


### Bug Fixes

* Fixed the slow response of the ListObjects call. ([#32](https://github.com/source-cooperative/data.source.coop/issues/32)) ([6afcf13](https://github.com/source-cooperative/data.source.coop/commit/6afcf13ec15b9cc79f5d6a2aef55b3d269a14e16))

## [0.1.19](https://github.com/source-cooperative/data.source.coop/compare/v0.1.18...v0.1.19) (2024-11-28)


### Bug Fixes

* Fixed issues in listing bucket at account level. ([#28](https://github.com/source-cooperative/data.source.coop/issues/28)) ([073d2ea](https://github.com/source-cooperative/data.source.coop/commit/073d2ea34fb5f4c00716605538c585a0a486588a))

## [0.1.18](https://github.com/source-cooperative/data.source.coop/compare/v0.1.17...v0.1.18) (2024-11-22)


### Bug Fixes

* check for empty access key id. ([#24](https://github.com/source-cooperative/data.source.coop/issues/24)) ([8df8242](https://github.com/source-cooperative/data.source.coop/commit/8df8242f1772705d672cf7594427333fc68627cb))

## [0.1.17](https://github.com/source-cooperative/data.source.coop/compare/v0.1.16...v0.1.17) (2024-11-13)


### Bug Fixes

* Fixed the issue in request authorization. Decoded the request path before its encoded again. ([#20](https://github.com/source-cooperative/data.source.coop/issues/20)) ([dc9eb84](https://github.com/source-cooperative/data.source.coop/commit/dc9eb84009eead0dbecd0990886f69811ca93abd))

Version 0.1.16
-------------
* Handled the boto3 download object with range request pattern `start-` which is a valid request to fetch the bytes from start till the total bytes. 

Version 0.1.15
--------------
* Added `CONTRIBUTING.md`, `CODE_OF_CONDUCT.md`, `CHANGELOG.rst`, Github issue templates, and Github pull request template.

Version 0.1.14
--------------
* Released initial open-source version of the project.
