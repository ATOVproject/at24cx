# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.1](https://github.com/AtoVproject/at24cx/compare/v0.1.0...v0.1.1) - 2024-11-08

### Added

- first implementation for AT24CM01

### Fixed

- remove erase function, not needed for at24x
- fix ack polling
- address bitwise fixes
- implement from I2cError for Error
- slightly increase the max poll ack time to 6ms
