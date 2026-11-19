# Conformance suites

Two public suites are run by hand against programs in this directory. Neither is vendored and CI
does not run them: each is a download, and Autobahn takes several minutes (D81). The latest
results are in docs/07, "Protocol conformance".

## JSONTestSuite

```bash
git clone --depth 1 https://github.com/nst/JSONTestSuite /tmp/JSONTestSuite
weby run --release tests/conformance/json_suite.wb /tmp/JSONTestSuite/test_parsing
```

It prints how many `y_` (must accept), `n_` (must reject) and `i_` (either) files were accepted
and rejected, then a `FAIL` line for each `y_` file rejected or `n_` file accepted.

## Autobahn WebSocket suite

The suite (`autobahntestsuite` 0.8.2) runs only on Python 2. Without Docker, a portable PyPy 2.7
from pypy.org works:

```bash
pypy -m ensurepip
pypy -m pip install --no-deps autobahntestsuite==0.8.2
pypy -m pip install six 'txaio<2.11' 'autobahn==0.10.9' 'zope.interface<5.5' constantly \
    'incremental<22' 'attrs<21.2' 'hyperlink<21' 'Automat<21' 'PyHamcrest<2' 'twisted==20.3.0' \
    'markupsafe<2' 'jinja2<3' 'werkzeug<2' 'klein<20' unittest2
```

Two edits to the installed package are needed. First, at the top of
`autobahntestsuite/__init__.py`, add `import txaio` and `txaio.use_twisted()`, because these
pinned versions do not pick a framework. Second, `pyOpenSSL` does not build here and the fuzzing
client does not use TLS, so in `autobahntestsuite/fuzzing.py` make `from twisted.internet import
ssl` optional (`try`/`except ImportError: ssl = None`).

Then start the echo server and point the fuzzing client at it:

```bash
weby build --release tests/conformance/ws_echo.wb -o /tmp/ws_echo
/tmp/ws_echo 9001 &
cat > fuzzingclient.json <<'JSON'
{
  "outdir": "./reports/servers",
  "servers": [{"agent": "weby", "url": "ws://127.0.0.1:9001/"}],
  "cases": ["*"],
  "exclude-cases": ["12.*", "13.*"],
  "exclude-agent-cases": {}
}
JSON
wstest -m fuzzingclient -s fuzzingclient.json
```

Sections 12 and 13 test `permessage-deflate` compression, which the server does not offer.
`reports/servers/index.html` has the results.
