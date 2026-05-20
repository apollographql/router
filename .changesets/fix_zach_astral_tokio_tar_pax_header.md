### Update `astral-tokio-tar` to 0.6.2 to address PAX header desynchronization advisory ([RUSTSEC-2026-0145](https://rustsec.org/advisories/RUSTSEC-2026-0145))

Bumps the `astral-tokio-tar` dependency from 0.6.1 to 0.6.2 to pick up the fix for [GHSA-3cv2-h65g-fgmm](https://github.com/astral-sh/tokio-tar/security/advisories/GHSA-3cv2-h65g-fgmm). Versions prior to 0.6.2 contain a PAX header interpretation bug that allows manipulated entries to be selectively visible or invisible during extraction with `astral-tokio-tar` versus other tar implementations, which an attacker could use to smuggle unexpected files onto a victim's filesystem during archive extraction.

By [@zachfettersmoore](https://github.com/zachfettersmoore) in https://github.com/apollographql/router/pull/9475
