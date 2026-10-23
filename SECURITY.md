# Security

Report vulnerabilities privately through GitHub Security Advisories. Do not publish credentials, provider tokens, or private request data in issues.

The model proxy must remain localhost-only, never log Authorization headers, secrets or prompt bodies, and never clobber externally modified Codex provider URLs. Security changes require tests.

This repository is independent of OpenAI.
