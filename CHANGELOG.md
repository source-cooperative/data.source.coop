# Changelog

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