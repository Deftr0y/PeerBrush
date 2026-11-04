# Public attestation verification pins

`trusted_root.jsonl` contains the GitHub and Sigstore public attestation roots
returned by the official GitHub CLI 2.102.0 `gh attestation trusted-root` command
on 2026-11-04. That command verifies the roots through TUF. These are public
verification keys, not application signing credentials.

`scripts/provision_release_verifier.py` pins this root file, the official Linux
CLI archive and its executable independently. It extracts only the regular
`bin/gh` member; the final native consumers still check their selected hashes,
workflow identity, exact source and successful native job.

Refresh these pins together after reviewing an official CLI release and its
published checksums. Never obtain authority from the artifact being verified.

Sources: [trusted-root command](https://cli.github.com/manual/gh_attestation_trusted-root)
and [CLI 2.102.0 release](https://github.com/cli/cli/releases/tag/v2.102.0).
