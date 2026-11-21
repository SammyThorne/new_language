# Time zone fixtures

Copied from tzdata 2026f (`/usr/share/zoneinfo` on Debian 13); the time zone database is in the
public domain. The `datetime` tests set `TZDIR` to this directory so they do not depend on the
machine's copy:

- `America/New_York`: daylight saving time in March and November.
- `Europe/London`: daylight saving time in March and October, changing at 01:00 UTC.
- `Australia/Lord_Howe`: a 30-minute daylight saving shift, in the southern hemisphere.
- `Etc/UTC`.

Each file's table ends in 2037; later instants use the POSIX TZ rule in its footer.
