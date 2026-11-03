# provider-python

An echo agent written in Python and served over the [Agent Client Protocol](https://agentclientprotocol.com): `echo/acp_echo.py` is the provider, `src/main.rs` a host that loads it through Guatiao's source of Python libraries and serves it on standard input and output, and `tests/python.rs` drives that host with `coqui_acp::Client`.

```sh
cargo test -p provider-python
```

The tests build the source, `guatiao-source-py`, from Cargo's checkout of Guatiao at the workspace's revision, against the Python `build.rs` found: `COQUI_ACP_TEST_PYTHON`, else `python3`, `python` or `py -3`. It has to have its shared library. Without one the build warns `NOT RUN` and the one test left is ignored. Elsewhere than a system Python, its `lib` directory goes on `LD_LIBRARY_PATH` (Linux) or `DYLD_FALLBACK_LIBRARY_PATH` (macOS).
