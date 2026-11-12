# Security

Thank you for taking the time to look. If you think you have found a security problem in Tubarr, please report it privately so it can be fixed before it is widely known.

## How to report

Use GitHub's [private vulnerability reporting](https://docs.github.com/en/code-security/security-advisories/guidance-on-reporting-and-writing-information-about-vulnerabilities/privately-reporting-a-security-vulnerability): open the repository's Security tab and choose "Report a vulnerability". This is the only channel for security reports.

Please do not open a public issue or pull request for a suspected vulnerability.

A useful report says which version or commit you tested, how Tubarr was set up (Docker or binary, sign-in method, whether it sits behind a reverse proxy), the steps to reproduce, and what an attacker could do with it. Logs and a proof of concept help, but please leave out real API keys, cookies and passwords.

## What to expect

Tubarr is maintained by a small team in spare time, so replies are best effort. You should get an acknowledgement within a week. We will keep you posted while a fix is prepared, agree on a disclosure date with you, and credit you in the release notes unless you would rather stay anonymous.

## Scope

In scope: the Tubarr service and its web UI, the Newznab, SABnzbd, admin and `/play` APIs, sign-in and sessions, the Docker image and the files it writes.

Out of scope: problems in yt-dlp, deno, Sonarr, Radarr, Jellyfin or YouTube itself (please report those upstream), and setups that expose Tubarr to the internet with sign-in turned off or `local_bypass` on behind a proxy, which the README warns against.

## Supported versions

Tubarr is pre-alpha. Only the latest release and the `main` branch get security fixes.

## Known accepted dependency risk

Tubarr 0.1.1 uses SQLx 0.9.0 with libsqlite3-sys 0.37.0 and bundled SQLite 3.51.3, which remains affected by [RUSTSEC-2026-0459](https://rustsec.org/advisories/RUSTSEC-2026-0459). Opening a crafted database together with a sibling WAL file whose page size differs can expose process memory through query results.

This is an accepted risk, not a patched or unaffected library. Keep database and WAL files, their storage and backups trusted. Never restore untrusted database files. Normal mapping JSON imports contain metadata, not raw database uploads.

The audit exception is limited to this advisory. A compatible official SQLx release that permits a fixed bundled SQLite dependency will allow the exception to be removed.
