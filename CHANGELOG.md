# Changelog

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
