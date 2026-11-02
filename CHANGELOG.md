# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- `coqui-acp`: drive any Agent Client Protocol version 1 agent from blocking code, with permission deciding, resume, extensions and line transports.
- `coqui_acp` for Dart: a client that speaks version 1 and version 2, whichever the agent selects.
- `coqui-acp-bridge`: a stdio ACP agent on the network under coqui-peer identity.
- `coqui-acp-bridge`: `serve --advertise` publishes a DNS-SD record, or a descriptor on loopback; `discover` finds both; `companion install` lets a browser extension ask, over native messaging.
