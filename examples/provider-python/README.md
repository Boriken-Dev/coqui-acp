# provider-python

Agents written in Python, for the [Agent Client Protocol](https://agentclientprotocol.com):

- `echo/acp_echo.py`, an `agent_provider`: `src/main.rs` is a host that loads it through Guatiao's source of Python libraries and serves it on standard input and output, and `tests/python.rs` drives that host with `coqui_acp::Client`.
- `harness/acp_echo_harness.py`, an `acp_harness` that speaks ACP itself over a Guatiao channel: `tests/harness.rs` loads it in the test process, as the Rust-declared kind of `coqui-acp-harness-kind`, and holds it to the same contract suite as that crate's Rust echo.

```sh
cargo test -p provider-python
```

The tests build the source, `guatiao-source-py`, from Cargo's checkout of Guatiao at the workspace's revision, against the Python `build.rs` found: `COQUI_ACP_TEST_PYTHON`, else `python3`, `python` or `py -3`. It has to have its shared library. Without one the build warns `NOT RUN` and the tests left are ignored. Elsewhere than a system Python, its `lib` directory goes on `LD_LIBRARY_PATH` (Linux) or `DYLD_FALLBACK_LIBRARY_PATH` (macOS), and for one that reports another prefix than its own (a python-build-standalone download reports `/install`), on the linker's path too: `RUSTFLAGS="-L native=<prefix>/lib"`.
